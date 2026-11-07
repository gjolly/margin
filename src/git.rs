// SPDX-License-Identifier: GPL-3.0-or-later
//! Collect a plain Git patch without invoking a shell or external diff helpers.
use std::ffi::OsString;
use std::path::Path;
use std::process::Command;

pub fn diff(args: &[OsString], cwd: Option<&Path>) -> Result<String, String> {
    let separator = args.iter().position(|a| a == "--").unwrap_or(args.len());
    // These modes don't provide a two-sided patch, or redirect it away from stdout.
    for arg in &args[..separator] {
        let arg = arg.to_string_lossy();
        let option = arg.split('=').next().unwrap_or(&arg);
        if matches!(option, "--name-only" | "--name-status" | "--raw" | "--numstat"
            | "--shortstat" | "--dirstat" | "--dirstat-by-file" | "--check"
            | "--output" | "--cc" | "-c" | "--combined-all-paths") {
            return Err(format!("{option} is not supported by the patch viewer. Use revision, comparison, and path arguments instead."));
        }
    }
    let mut command = Command::new("git");
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    command.args(["--no-pager", "diff"]);
    command.args(&args[..separator]);
    // Last option wins; place our format options before Git's path separator.
    command.args([
        "--patch", "--no-color", "--no-ext-diff", "--no-textconv",
        "--no-relative", "--src-prefix=a/", "--dst-prefix=b/",
        "--line-prefix=", "--word-diff=none", "--no-quiet",
    ]);
    command.args(&args[separator..]);
    let output = command.output().map_err(|e| format!("Could not run git: {e}"))?;
    // --exit-code and --no-index use status 1 for differences, not failure.
    if !matches!(output.status.code(), Some(0 | 1)) {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
    }
    if output.status.code() == Some(1) && output.stdout.is_empty() && !output.stderr.is_empty() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
    }
    String::from_utf8(output.stdout)
        .map_err(|_| "Git returned a non-UTF-8 patch; this viewer currently supports UTF-8 text.".into())
}

#[cfg(test)]
#[path = "git_tests.rs"]
mod tests;
