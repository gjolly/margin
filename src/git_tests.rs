// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;
use std::fs;
use std::sync::atomic::{AtomicU64, Ordering};
use crate::diff::{FileStatus, parse};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Repo(std::path::PathBuf);

impl Repo {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("margin-test-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir(&dir).unwrap();
        let repo = Self(dir);
        repo.git(&["init", "-q"]);
        repo
    }

    fn git(&self, args: &[&str]) {
        let output = Command::new("git").current_dir(&self.0)
            .args(["-c", "user.name=Test", "-c", "user.email=test@example.invalid",
                   "-c", "commit.gpgsign=false"])
            .args(args).output().unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    }

    fn write(&self, name: &str, text: &str) {
        fs::write(self.0.join(name), text).unwrap();
    }

    fn patch(&self, args: &[&str]) -> String {
        diff(&args.iter().map(OsString::from).collect::<Vec<_>>(), Some(&self.0)).unwrap()
    }

    fn commit(&self) {
        self.git(&["add", "."]);
        self.git(&["commit", "-qm", "fixture"]);
    }
}

impl Drop for Repo {
    fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); }
}

#[test]
fn working_tree_index_revisions_and_path_separator_match_git() {
    let r = Repo::new();
    r.write("a file.txt", "base\n");
    r.write("other.txt", "base\n");
    r.commit();
    r.write("a file.txt", "staged\n");
    r.git(&["add", "a file.txt"]);
    r.write("a file.txt", "working\n");
    r.write("other.txt", "changed\n");
    let patch = r.patch(&["--", "a file.txt"]);
    let files = parse(&patch);
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].path(), "a file.txt");
    assert!(patch.contains("-staged\n+working"));
    let staged = r.patch(&["--cached", "--", "a file.txt"]);
    assert!(staged.contains("-base\n+staged"));
    r.commit();
    let revisions = r.patch(&["HEAD~1", "HEAD", "--", "a file.txt"]);
    assert!(revisions.contains("-base\n+working"));
    assert!(r.patch(&[]).is_empty());
}

#[test]
fn added_deleted_renamed_and_binary_files_parse() {
    let r = Repo::new();
    r.write("delete.txt", "deleted\n");
    r.write("rename.txt", "unchanged\n");
    r.commit();
    r.git(&["mv", "rename.txt", "renamed.txt"]);
    r.git(&["rm", "-q", "delete.txt"]);
    r.write("added.txt", "added\n");
    fs::write(r.0.join("binary.bin"), [0, 1, 2]).unwrap();
    r.git(&["add", "."]);
    let files = parse(&r.patch(&["--cached", "-M"]));
    assert_eq!(files.len(), 4);
    assert!(files.iter().any(|f| f.status == FileStatus::Added && f.path() == "added.txt"));
    assert!(files.iter().any(|f| f.status == FileStatus::Deleted));
    assert!(files.iter().any(|f| f.status == FileStatus::Renamed));
    assert!(files.iter().any(|f| f.binary));
}

#[test]
fn exit_code_one_is_a_diff_and_invalid_revisions_are_errors() {
    let r = Repo::new();
    r.write("file.txt", "before\n");
    r.commit();
    r.write("file.txt", "after\n");
    assert_eq!(parse(&r.patch(&["--exit-code"])).len(), 1);
    r.write("old.txt", "old\n");
    r.write("new.txt", "new\n");
    assert_eq!(parse(&r.patch(&["--no-index", "--", "old.txt", "new.txt"])).len(), 1);
    assert!(diff(&[OsString::from("missing-revision")], Some(&r.0)).is_err());
    assert!(diff(&[OsString::from("--name-only")], Some(&r.0)).is_err());
}

#[test]
fn formatting_is_plain_even_with_color_and_custom_prefixes() {
    let r = Repo::new();
    r.write("file.txt", "before\n");
    r.commit();
    r.write("file.txt", "after\n");
    r.git(&["config", "diff.noprefix", "true"]);
    let patch = r.patch(&["--color=always", "--src-prefix=old/", "--dst-prefix=new/"]);
    assert!(!patch.contains('\u{1b}'));
    assert!(patch.contains("--- a/file.txt"));
    assert_eq!(parse(&patch)[0].path(), "file.txt");
}

#[test]
fn outside_repository_returns_an_error() {
    let r = Repo::new();
    fs::remove_dir_all(r.0.join(".git")).unwrap();
    assert!(diff(&[], Some(&r.0)).is_err());
}
