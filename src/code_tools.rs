// SPDX-License-Identifier: GPL-3.0-or-later
//! Bounded, read-only working-tree inspection. No model-supplied commands.
use ignore::WalkBuilder;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{collections::BTreeSet, fs::File, io::Read, path::{Component, Path, PathBuf}, process::Command, time::{Duration, Instant}};

pub const RESULT_LIMIT: usize = 32 * 1024;
const FILE_LIMIT: usize = 1024 * 1024;

pub fn root(cwd: &Path) -> PathBuf {
    Command::new("git").args(["rev-parse", "--show-toplevel"]).current_dir(cwd).output().ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| PathBuf::from(s.trim_end_matches(['\n', '\r'])))
        .unwrap_or_else(|| cwd.to_path_buf())
}

pub struct CodeTools {
    root: PathBuf,
    files: BTreeSet<String>,
    inventory_truncated: bool,
}

pub fn definitions() -> Value {
    json!([
        {"type":"function","function":{"name":"list_files","description":"List eligible local working-tree files, relative to the code root. Ignored files and .git are excluded. Use offset to page.","parameters":{"type":"object","properties":{"path":{"type":"string","description":"Optional directory prefix"},"offset":{"type":"integer","minimum":0}},"additionalProperties":false}}},
        {"type":"function","function":{"name":"search_code","description":"Search literal text in current local UTF-8 code. Returns path, line number, and matching line; results and scan size are bounded.","parameters":{"type":"object","properties":{"query":{"type":"string"},"path":{"type":"string","description":"Optional file or directory prefix"}},"required":["query"],"additionalProperties":false}}},
        {"type":"function","function":{"name":"read_file","description":"Read a local working-tree UTF-8 file with 1-based line numbers. At most 400 lines per call. This is current local content, not a historical Git snapshot.","parameters":{"type":"object","properties":{"path":{"type":"string"},"start_line":{"type":"integer","minimum":1},"end_line":{"type":"integer","minimum":1}},"required":["path"],"additionalProperties":false}}}
    ])
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListArgs { #[serde(default)] path: String, #[serde(default)] offset: usize }
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchArgs { query: String, #[serde(default)] path: String }
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadArgs { path: String, start_line: Option<usize>, end_line: Option<usize> }

impl CodeTools {
    pub fn new(root: &Path) -> Result<Self, String> {
        let root = root.canonicalize().map_err(|_| "Code root is unavailable.")?;
        let mut files = BTreeSet::new();
        let mut bytes = 0;
        let mut inventory_truncated = false;
        let start = Instant::now();
        let walker = WalkBuilder::new(&root).hidden(false).follow_links(false)
            .filter_entry(|entry| entry.file_name() != ".git").build();
        for (count, entry) in walker.enumerate() {
            if count >= 100_000 || bytes >= 8 * 1024 * 1024 || start.elapsed() > Duration::from_secs(10) {
                inventory_truncated = true;
                break;
            }
            let Ok(entry) = entry else { continue };
            if entry.file_type().is_some_and(|t| t.is_file() || t.is_symlink()) {
                let Ok(relative) = entry.path().strip_prefix(&root) else { continue };
                let Some(relative) = relative.to_str() else { continue };
                bytes += relative.len();
                files.insert(relative.to_owned());
            }
        }
        Ok(Self { root, files, inventory_truncated })
    }

    fn validate(path: &str) -> Result<(), String> {
        if Path::new(path).components().any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
            || Path::new(path).components().any(|c| c.as_os_str() == ".git")
        { return Err("Path must stay within the code root and outside .git.".into()); }
        Ok(())
    }

    fn relative(path: &str) -> Result<String, String> {
        Self::validate(path)?;
        Ok(Path::new(path).components().filter_map(|c| match c {
            Component::Normal(part) => part.to_str(),
            _ => None,
        }).collect::<Vec<_>>().join("/"))
    }

    fn read(&self, path: &str) -> Result<(String, bool), String> {
        let path = Self::relative(path)?;
        if !self.files.contains(&path) { return Err("File is missing, ignored, or outside the eligible inventory.".into()); }
        let resolved = self.root.join(&path).canonicalize().map_err(|_| "File is unavailable.")?;
        if !resolved.starts_with(&self.root) || resolved.strip_prefix(&self.root).unwrap().components().any(|c| c.as_os_str() == ".git") {
            return Err("Symlink target is outside the permitted code tree.".into());
        }
        // Check eligibility of the target as well as the symlink itself.
        let relative = resolved.strip_prefix(&self.root).unwrap().to_str().ok_or("Non-UTF-8 path.")?;
        if !self.files.contains(relative) { return Err("Symlink target is not an eligible code file.".into()); }
        if !resolved.is_file() { return Err("Path is not a regular file.".into()); }
        let file = File::open(resolved).map_err(|_| "File cannot be read.")?;
        let mut bytes = Vec::new();
        file.take(FILE_LIMIT as u64 + 1).read_to_end(&mut bytes).map_err(|_| "File cannot be read.")?;
        let truncated = bytes.len() > FILE_LIMIT;
        bytes.truncate(FILE_LIMIT);
        if bytes.contains(&0) { return Err("Binary files are not supported.".into()); }
        // A capped read may split the final UTF-8 character.
        if truncated { while std::str::from_utf8(&bytes).is_err_and(|e| e.error_len().is_none()) { bytes.pop(); } }
        let text = String::from_utf8(bytes).map_err(|_| "File is not UTF-8 text.")?;
        Ok((text, truncated))
    }

    pub fn call(&self, name: &str, args: &str) -> Value {
        match self.execute(name, args) {
            Ok((text, truncated)) => bounded(text, truncated || self.inventory_truncated),
            Err(error) => json!({"error":error}),
        }
    }

    fn execute(&self, name: &str, args: &str) -> Result<(String, bool), String> {
        let invalid = |_| "Invalid tool arguments.".to_owned();
        match name {
            "list_files" => {
                let args: ListArgs = serde_json::from_str(args).map_err(invalid)?;
                let prefix = Self::relative(&args.path)?;
                let mut matches = self.files.iter().filter(|p| under(p, &prefix)).skip(args.offset);
                let page: Vec<_> = matches.by_ref().take(200).cloned().collect();
                Ok((page.join("\n"), matches.next().is_some()))
            }
            "read_file" => {
                let args: ReadArgs = serde_json::from_str(args).map_err(invalid)?;
                let start = args.start_line.unwrap_or(1);
                let end = args.end_line.unwrap_or(start.saturating_add(199));
                if start == 0 || end < start || end - start >= 400 { return Err("Request 1–400 lines using a valid 1-based range.".into()); }
                let (text, truncated) = self.read(&args.path)?;
                let result = text.lines().enumerate().skip(start - 1).take(end - start + 1)
                    .map(|(i, line)| format!("{}: {line}", i + 1)).collect::<Vec<_>>().join("\n");
                Ok((result, truncated))
            }
            "search_code" => {
                let args: SearchArgs = serde_json::from_str(args).map_err(invalid)?;
                let prefix = Self::relative(&args.path)?;
                if args.query.is_empty() || args.query.len() > 4096 { return Err("Search query must contain 1–4096 bytes.".into()); }
                let mut result = String::new();
                let mut scanned = 0;
                let mut matches = 0;
                let mut truncated = false;
                let start = Instant::now();
                for path in self.files.iter().filter(|p| under(p, &prefix)) {
                    if scanned >= 64 * 1024 * 1024 || start.elapsed() > Duration::from_secs(10) { truncated = true; break; }
                    // Count binary and invalid UTF-8 reads toward the scan budget too.
                    let size = self.root.join(path).metadata().map(|m| m.len().min(FILE_LIMIT as u64 + 1) as usize).unwrap_or(0);
                    if scanned + size > 64 * 1024 * 1024 { truncated = true; break; }
                    scanned += size;
                    let Ok((text, capped)) = self.read(path) else { continue };
                    truncated |= capped;
                    for (i, line) in text.lines().enumerate() {
                        if line.contains(&args.query) {
                            result.push_str(&format!("{path}:{}: {line}\n", i + 1));
                            matches += 1;
                            if matches >= 100 || result.len() >= RESULT_LIMIT { return Ok((result, true)); }
                        }
                    }
                }
                Ok((result, truncated))
            }
            _ => Err("Unknown tool. Only list_files, search_code, and read_file are available.".into()),
        }
    }
}

fn under(path: &str, prefix: &str) -> bool {
    prefix.is_empty() || path == prefix || path.starts_with(&format!("{}/", prefix.trim_end_matches('/')))
}

pub fn truncate(text: &mut String, limit: usize) {
    if text.len() > limit {
        let mut end = limit;
        while !text.is_char_boundary(end) { end -= 1; }
        text.truncate(end);
    }
}

fn bounded(mut text: String, mut truncated: bool) -> Value {
    if text.len() > RESULT_LIMIT - 128 { truncate(&mut text, RESULT_LIMIT - 128); truncated = true; }
    loop {
        let result = json!({"text":text,"truncated":truncated,"source":"current working tree"});
        if result.to_string().len() <= RESULT_LIMIT { return result; }
        let limit = text.len() * 3 / 4;
        truncate(&mut text, limit);
        truncated = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tools_are_read_only_bounded_and_respect_ignores() {
        let dir = tempfile::tempdir().unwrap();
        assert!(Command::new("git").args(["init", "--quiet"]).current_dir(dir.path()).status().unwrap().success());
        std::fs::write(dir.path().join(".gitignore"), "secret.txt\n").unwrap();
        std::fs::write(dir.path().join("secret.txt"), "SECRET").unwrap();
        std::fs::write(dir.path().join("file with spaces.rs"), "first\nneedle\nthird\n").unwrap();
        std::fs::write(dir.path().join("binary"), b"\0binary").unwrap();
        std::fs::write(dir.path().join("invalid"), [255]).unwrap();
        std::fs::write(dir.path().join("large"), "é".repeat(40_000)).unwrap();
        let tools = CodeTools::new(dir.path()).unwrap();
        let listing = tools.call("list_files", "{}");
        assert!(listing["text"].as_str().unwrap().contains("file with spaces.rs"));
        assert!(!listing["text"].as_str().unwrap().contains("secret.txt"));
        assert!(tools.call("read_file", r#"{"path":"secret.txt"}"#)["error"].is_string());
        assert!(tools.call("read_file", r#"{"path":"../outside"}"#)["error"].is_string());
        assert!(tools.call("read_file", r#"{"path":".git/config"}"#)["error"].is_string());
        for path in ["binary", "invalid", "missing"] {
            assert!(tools.call("read_file", &json!({"path":path}).to_string())["error"].is_string());
        }
        assert!(tools.call("shell", "{}")["error"].is_string());
        assert!(tools.call("list_files", r#"{"command":"bad"}"#)["error"].is_string());
        assert!(tools.call("search_code", r#"{"query":"needle"}"#)["text"].as_str().unwrap().contains("file with spaces.rs:2: needle"));
        assert_eq!(tools.call("read_file", r#"{"path":"file with spaces.rs","start_line":2,"end_line":2}"#)["text"], "2: needle");
        let large = tools.call("read_file", r#"{"path":"large"}"#);
        assert_eq!(large["truncated"], true);
        assert!(large.to_string().len() <= RESULT_LIMIT);
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_cannot_escape_or_bypass_ignore_rules() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();
        assert!(Command::new("git").args(["init", "--quiet"]).current_dir(dir.path()).status().unwrap().success());
        std::fs::write(dir.path().join(".gitignore"), "secret\n").unwrap();
        std::fs::write(dir.path().join("secret"), "secret content").unwrap();
        std::fs::write(dir.path().join("code.rs"), "public code").unwrap();
        symlink(outside.path(), dir.path().join("escape")).unwrap();
        symlink(dir.path().join("secret"), dir.path().join("bypass")).unwrap();
        symlink(dir.path().join("code.rs"), dir.path().join("allowed")).unwrap();
        let tools = CodeTools::new(dir.path()).unwrap();
        assert!(tools.call("read_file", r#"{"path":"escape"}"#)["error"].is_string());
        assert!(tools.call("read_file", r#"{"path":"bypass"}"#)["error"].is_string());
        assert_eq!(tools.call("read_file", r#"{"path":"allowed"}"#)["text"], "1: public code");
        let nested = dir.path().join("nested");
        std::fs::create_dir(&nested).unwrap();
        assert_eq!(root(&nested), dir.path());
    }
}
