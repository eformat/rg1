//! Unified-diff stream parsing: git `log -p` / `format-patch` commit streams
//! and plain diffs, split into per-commit hunks.

use anyhow::Result;
use regex::Regex;
use std::path::Path;
use std::process::Command;

pub struct Hunk {
    pub commit: Option<String>,
    /// Post-image (target) file path.
    pub file: String,
    pub is_new: bool,
    pub is_deleted: bool,
    pub source_start: usize,
    pub source_length: usize,
    pub target_start: usize,
    pub target_length: usize,
    pub section_header: String,
    /// Post-image line of the first changed (`+`) line, or the first context
    /// line when the hunk is a pure deletion.
    pub first_target_line: u64,
    /// The hunk text including its `@@` header, source-preserving.
    pub body: String,
}

/// Run `git log -p` in a repository directory and return its output as a stream.
pub fn git_log_stream(repo: &Path) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["log", "-p", "--no-color", "--no-ext-diff", "--full-index"])
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_NO_LAZY_FETCH", "1")
        .output()?;
    if !out.status.success() {
        anyhow::bail!(
            "git log -p failed in {}: {}",
            repo.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Split a diff stream into (commit hash, segment text) pairs. Plain diffs
/// yield a single `(None, text)` segment.
fn split_commits(text: &str) -> Vec<(Option<String>, String)> {
    let log_re = Regex::new(r"^commit ([0-9a-f]{40})").unwrap();
    let mbox_re = Regex::new(r"^From ([0-9a-f]{40}) ").unwrap();
    let is_log = text.lines().any(|l| log_re.is_match(l));
    let is_mbox = text.lines().any(|l| mbox_re.is_match(l));

    let boundary = |line: &str| -> Option<String> {
        if is_log {
            log_re.captures(line).map(|c| c[1].to_string())
        } else if is_mbox {
            mbox_re.captures(line).map(|c| c[1].to_string())
        } else {
            None
        }
    };

    let mut segments = Vec::new();
    let mut current_commit: Option<String> = None;
    let mut current = String::new();
    for line in text.lines() {
        if let Some(commit) = boundary(line) {
            if !current.trim().is_empty() {
                segments.push((current_commit.take(), std::mem::take(&mut current)));
            }
            current_commit = Some(commit);
            current.push_str(line);
            current.push('\n');
        } else {
            current.push_str(line);
            current.push('\n');
        }
    }
    if !current.trim().is_empty() {
        segments.push((current_commit, current));
    }
    segments
}

fn strip_prefix(p: &str) -> String {
    p.strip_prefix("b/")
        .or_else(|| p.strip_prefix("a/"))
        .unwrap_or(p)
        .to_string()
}

/// Parse a diff stream into hunks. Hunks whose text fails to parse are skipped.
pub fn parse_stream(text: &str) -> Vec<Hunk> {
    let mut out = Vec::new();
    for (commit, segment) in split_commits(text) {
        let mut patch = unidiff::PatchSet::new();
        if patch.parse(&segment).is_err() {
            continue;
        }
        for file in patch.files() {
            let file_path = strip_prefix(&file.path());
            for hunk in file.hunks() {
                let mut body = format!(
                    "@@ -{},{} +{},{} @@ {}\n",
                    hunk.source_start,
                    hunk.source_length,
                    hunk.target_start,
                    hunk.target_length,
                    hunk.section_header.trim_end()
                );
                let mut first_added_line: Option<u64> = None;
                let mut first_context_line: Option<u64> = None;
                for line in hunk.lines() {
                    body.push_str(&line.line_type);
                    body.push_str(line.value.trim_end_matches('\n'));
                    body.push('\n');
                    if line.is_added() && first_added_line.is_none() {
                        first_added_line = line.target_line_no.map(|t| t as u64);
                    }
                    if line.is_context() && first_context_line.is_none() {
                        first_context_line = line.target_line_no.map(|t| t as u64);
                    }
                }
                // -W wants the first changed (+) line; pure deletions fall back
                // to the first context line.
                let first_target_line = first_added_line
                    .or(first_context_line)
                    .unwrap_or(hunk.target_start.max(1) as u64);
                out.push(Hunk {
                    commit: commit.clone(),
                    file: file_path.clone(),
                    is_new: file.is_added_file(),
                    is_deleted: file.is_removed_file(),
                    source_start: hunk.source_start,
                    source_length: hunk.source_length,
                    target_start: hunk.target_start,
                    target_length: hunk.target_length,
                    section_header: hunk.section_header.clone(),
                    first_target_line,
                    body,
                });
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
commit 0123456789abcdef0123456789abcdef01234567
Author: Ada <ada@example.com>
Date:   Mon Oct 5 12:00:00 2026 +0000

    fix greeting

diff --git a/src/hello.py b/src/hello.py
index 1111111..2222222 100644
--- a/src/hello.py
+++ b/src/hello.py
@@ -1,4 +1,5 @@
 import os
 
 def greet(name):
-    return \"hi\"
+    # greet the user warmly
+    return f\"hi {name}\"
";

    #[test]
    fn parses_git_log_stream() {
        let hunks = parse_stream(SAMPLE);
        assert_eq!(hunks.len(), 1);
        let h = &hunks[0];
        assert_eq!(
            h.commit.as_deref(),
            Some("0123456789abcdef0123456789abcdef01234567")
        );
        assert_eq!(h.file, "src/hello.py");
        assert_eq!(h.target_start, 1);
        assert_eq!(h.target_length, 5);
        assert!(h.body.contains("# greet the user warmly"));
        assert!(h.body.starts_with("@@ -1,4 +1,5 @@"));
        assert!(h.body.contains("-    return \"hi\""));
        assert!(h.body.contains("+    return f\"hi {name}\""));
    }

    #[test]
    fn parses_plain_diff() {
        let plain = SAMPLE.split_once("diff --git").map(|(_, rest)| format!("diff --git{rest}")).unwrap();
        let hunks = parse_stream(&plain);
        assert_eq!(hunks.len(), 1);
        assert!(hunks[0].commit.is_none());
    }
}
