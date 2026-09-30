//! Hardened git blob access for `-W` enclosing-function context.
//!
//! Fixed-argument git invocations (no shell), hostile-repo defenses
//! (no lazy fetch from promisor remotes, no object replacement,
//! work-tree-relative paths only), and a hard blob size cap.

use crate::code::{self, FnSpan};
use std::path::Path;
use std::process::Command;

pub const MAX_BLOB_BYTES: u64 = 10 * 1024 * 1024;

/// Why `-W` context could not be attached (the hunk is still judged alone).
pub type FallbackReason = &'static str;

pub const DELETED_FILE: FallbackReason = "deleted_file";
pub const SOURCE_MISMATCH: FallbackReason = "source_mismatch";
pub const OUTSIDE_FUNCTION: FallbackReason = "outside_function";
pub const FUNCTION_IN_HUNK: FallbackReason = "function_in_hunk";
pub const SYNTAX_ERROR: FallbackReason = "syntax_error";
pub const TOO_LARGE: FallbackReason = "too_large";
pub const NO_GIT: FallbackReason = "no_git_repo";
pub const NO_LANG: FallbackReason = "unknown_language";

/// Find the repo root containing `start` (via `git rev-parse --show-toplevel`).
pub fn find_repo(start: &Path) -> Option<PathBuf> {
    let out = Command::new("git")
        .arg("-C")
        .arg(start)
        .args(["rev-parse", "--show-toplevel"])
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(PathBuf::from(s))
    }
}

use std::path::PathBuf;

/// Read a blob at `<commit>:<path>` with hardening applied. Returns lossy UTF-8.
fn read_blob(repo: &Path, commit: &str, path: &str) -> Result<String, FallbackReason> {
    // Work-tree-relative paths only: no absolute paths, no traversal.
    if path.is_empty()
        || path.contains('\0')
        || path.starts_with('/')
        || path.split('/').any(|seg| seg == "..")
    {
        return Err(SOURCE_MISMATCH);
    }
    if !commit.chars().all(|c| c.is_ascii_hexdigit()) || commit.len() < 7 {
        return Err(SOURCE_MISMATCH);
    }
    let rev = format!("{commit}:{path}");
    let out = Command::new("git")
        .arg("--no-replace-objects")
        .arg("-C")
        .arg(repo)
        .args(["cat-file", "blob", &rev])
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_LFS_SKIP_SMUDGE", "1")
        .output()
        .map_err(|_| NO_GIT)?;
    if !out.status.success() {
        return Err(SOURCE_MISMATCH);
    }
    if out.stdout.len() as u64 > MAX_BLOB_BYTES {
        return Err(TOO_LARGE);
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Find the function in the post-image blob that contains `target_line`.
pub fn enclosing_function(
    repo: &Path,
    commit: &str,
    path: &str,
    target_line: u64,
    max_chars: usize,
) -> Result<FnSpan, FallbackReason> {
    let blob = read_blob(repo, commit, path)?;
    let Some(lang) = code::lang_for_path(Path::new(path)) else {
        return Err(NO_LANG);
    };
    let fns = code::extract_functions(&blob, lang);
    if fns.is_empty() {
        return Err(SYNTAX_ERROR);
    }
    let Some(f) = fns.into_iter().find(|f| f.start <= target_line && target_line <= f.end) else {
        return Err(OUTSIDE_FUNCTION);
    };
    if f.end - f.start + 1 == 0 {
        return Err(OUTSIDE_FUNCTION);
    }
    if f.body.chars().count() > max_chars {
        return Err(TOO_LARGE);
    }
    Ok(f)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_path_traversal() {
        let repo = Path::new("/tmp");
        assert_eq!(
            read_blob(repo, "abc123", "../../etc/passwd"),
            Err(SOURCE_MISMATCH)
        );
        assert_eq!(read_blob(repo, "abc123", "/etc/passwd"), Err(SOURCE_MISMATCH));
    }
}
