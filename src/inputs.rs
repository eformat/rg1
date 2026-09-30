//! File discovery (`ignore` walker — ripgrep's traversal crate) and record
//! construction for the text input modes.

use crate::cli::{Args, InputMode};
use crate::code;
use crate::diff;
use crate::gitctx;
use crate::prefilter::Prefilter;
use crate::record::{HunkMeta, Record, RecordKind};
use anyhow::{bail, Result};
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};

pub const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;

pub struct Prepared {
    pub records: Vec<Record>,
    pub files_read: u64,
    /// All records built, before prefiltering.
    pub total: u64,
    /// Files that could not be read (too large / IO error).
    pub errors: u64,
    /// Binary (NUL-containing) files skipped.
    pub binary_skipped: u64,
    /// `-W` fallback reasons, counted by reason.
    pub ctx_fallbacks: HashMap<&'static str, u64>,
}

enum Source {
    /// A regular file's content (path None = stdin).
    Text {
        path: Option<PathBuf>,
        content: String,
    },
    /// A unified-diff stream (git log -p / format-patch / plain diff).
    Diff {
        path: Option<PathBuf>,
        content: String,
        repo: Option<PathBuf>,
    },
}

fn note(msg: &str) {
    eprintln!("rg1: {msg}");
}

/// Count a record (pre-filter total) and optionally keep it.
pub(crate) fn keep(p: &mut Prepared, rec: Record, filter: Option<&Prefilter>) {
    p.total += 1;
    if let Some(pf) = filter {
        if !pf.is_match(&rec.body) {
            return;
        }
    }
    p.records.push(rec);
}

/// Discover inputs and build records (optionally pre-filtered).
pub fn prepare(args: &Args, filter: Option<&Prefilter>) -> Result<Prepared> {
    let mode = args.input_mode();
    let sources = gather(args, &mode)?;
    let mut p = Prepared {
        records: Vec::new(),
        files_read: 0,
        total: 0,
        errors: 0,
        binary_skipped: 0,
        ctx_fallbacks: HashMap::new(),
    };

    for source in sources {
        match source {
            Source::Text { path, content } => {
                p.files_read += 1;
                match mode {
                    InputMode::Lines => lines_records(args, &path, &content, filter, &mut p),
                    InputMode::Para => para_records(&path, &content, filter, &mut p),
                    InputMode::Whole => {
                        text_records(&path, &content, RecordKind::Whole, filter, &mut p)
                    }
                    InputMode::Chunks { size, overlap } => {
                        chunk_records(&path, &content, size, overlap, filter, &mut p)
                    }
                    InputMode::Jsonl => crate::data::jsonl_records(
                        &path,
                        &content,
                        args.field.as_deref(),
                        filter,
                        &mut p,
                    ),
                    InputMode::Csv => crate::data::csv_records(
                        &path,
                        &content,
                        args.field.as_deref(),
                        filter,
                        &mut p,
                    ),
                    InputMode::Functions => function_records(args, &path, &content, filter, &mut p),
                    InputMode::Diff => unreachable!("diff sources are handled separately"),
                }
            }
            Source::Diff {
                path,
                content,
                repo,
            } => {
                p.files_read += 1;
                diff_records(args, &path, &content, repo, filter, &mut p);
            }
        }
    }
    Ok(p)
}

/// Gather input sources from paths/stdin (diff dirs become `git log -p` streams).
fn gather(args: &Args, mode: &InputMode) -> Result<Vec<Source>> {
    let mut sources = Vec::new();
    if args.paths.is_empty() {
        let content = read_stdin()?;
        match mode {
            InputMode::Diff => sources.push(Source::Diff {
                path: None,
                content,
                repo: args
                    .repo
                    .clone()
                    .or_else(|| gitctx::find_repo(&std::env::current_dir().unwrap_or_default())),
            }),
            InputMode::Functions => {
                bail!("no input files for --functions (pass paths on the command line)");
            }
            _ => sources.push(Source::Text {
                path: None,
                content,
            }),
        }
        return Ok(sources);
    }

    for path in &args.paths {
        if path.as_os_str() == "-" {
            let content = read_stdin()?;
            if let InputMode::Diff = mode {
                sources.push(Source::Diff {
                    path: None,
                    content,
                    repo: args.repo.clone().or_else(|| {
                        gitctx::find_repo(&std::env::current_dir().unwrap_or_default())
                    }),
                });
            } else {
                sources.push(Source::Text {
                    path: None,
                    content,
                });
            }
            continue;
        }
        let meta = std::fs::metadata(path);
        let is_dir = meta.as_ref().map(|m| m.is_dir()).unwrap_or(false);
        if is_dir && *mode == InputMode::Diff {
            let content = diff::git_log_stream(path)?;
            sources.push(Source::Diff {
                path: Some(path.clone()),
                content,
                repo: args.repo.clone().or_else(|| gitctx::find_repo(path)),
            });
            continue;
        }
        if is_dir {
            for file in walk_dir(args, path) {
                if let Some(src) = read_source(&file, mode, args) {
                    sources.push(src);
                }
            }
        } else if let Some(src) = read_source(path, mode, args) {
            sources.push(src);
        }
    }
    Ok(sources)
}

fn read_source(path: &Path, mode: &InputMode, args: &Args) -> Option<Source> {
    match mode {
        InputMode::Diff => {
            let content = match std::fs::read_to_string(path) {
                Ok(c) => c,
                Err(e) => {
                    note(&format!("{}: {e}", path.display()));
                    return None;
                }
            };
            let repo = args.repo.clone().or_else(|| gitctx::find_repo(path));
            Some(Source::Diff {
                path: Some(path.to_path_buf()),
                content,
                repo,
            })
        }
        InputMode::Functions => {
            let content = read_text(path)?;
            Some(Source::Text {
                path: Some(path.to_path_buf()),
                content,
            })
        }
        _ => {
            let content = read_text(path)?;
            Some(Source::Text {
                path: Some(path.to_path_buf()),
                content,
            })
        }
    }
}

/// gitignore-aware walk (the `ignore` crate — ripgrep's traversal), sorted.
fn walk_dir(args: &Args, root: &Path) -> Vec<PathBuf> {
    let mut builder = ignore::WalkBuilder::new(root);
    builder.hidden(!args.hidden);
    builder.ignore(!args.no_ignore);
    builder.git_ignore(!args.no_ignore);
    builder.git_global(!args.no_ignore);
    builder.git_exclude(!args.no_ignore);
    builder.parents(!args.no_ignore);
    builder.follow_links(false);
    if !args.globs.is_empty() || !args.excludes.is_empty() {
        let mut ov = ignore::overrides::OverrideBuilder::new(root);
        for g in &args.globs {
            let _ = ov.add(g);
        }
        for x in &args.excludes {
            let _ = ov.add(&format!("!{x}"));
        }
        if let Ok(overrides) = ov.build() {
            builder.overrides(overrides);
        }
    }
    let mut files = Vec::new();
    for entry in builder.build() {
        let Ok(entry) = entry else {
            p_note_walk();
            continue;
        };
        let Some(ft) = entry.file_type() else {
            continue;
        };
        if !ft.is_file() || ft.is_symlink() {
            continue;
        }
        files.push(entry.into_path());
    }
    files.sort();
    files.dedup();
    files
}

fn p_note_walk() {
    note("walk error (skipping)");
}

/// Read a file as text; skips binary (NUL) and oversized files with a note.
fn read_text(path: &Path) -> Option<String> {
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) => {
            note(&format!("{}: {e}", path.display()));
            return None;
        }
    };
    if meta.len() > MAX_FILE_BYTES {
        note(&format!(
            "{}: file too large ({} bytes, max {MAX_FILE_BYTES}); skipping",
            path.display(),
            meta.len()
        ));
        return None;
    }
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            note(&format!("{}: {e}", path.display()));
            return None;
        }
    };
    if bytes.contains(&0) {
        return None; // binary: skipped silently (grep -I behavior)
    }
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

fn read_stdin() -> Result<String> {
    let mut buf = Vec::new();
    std::io::stdin().lock().read_to_end(&mut buf)?;
    if buf.len() as u64 > MAX_FILE_BYTES {
        bail!("stdin too large (max {MAX_FILE_BYTES} bytes)");
    }
    if buf.contains(&0) {
        return Ok(String::new());
    }
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// Line-mode records via the ripgrep engine; `-C` builds a marked context state.
fn lines_records(
    args: &Args,
    path: &Option<PathBuf>,
    content: &str,
    filter: Option<&Prefilter>,
    p: &mut Prepared,
) {
    let pattern = match filter {
        Some(pf) => pf.line_pattern().to_string(),
        None => crate::prefilter::MATCH_ALL.to_string(),
    };
    let hits = match crate::search::search_lines(content.as_bytes(), &pattern, true) {
        Ok(h) => h,
        Err(e) => {
            note(&format!("search error: {e}"));
            p.errors += 1;
            return;
        }
    };
    if hits.is_empty() {
        return;
    }
    let lines: Vec<&str> = content.lines().collect();
    let ctx = args.context.unwrap_or(0);
    for h in hits {
        let mut rec = Record::new(
            path.clone(),
            RecordKind::Line,
            h.line_number,
            h.line_number,
            h.line,
        );
        if ctx > 0 {
            let lo = (h.line_number as usize).saturating_sub(ctx).max(1);
            let hi = (h.line_number as usize + ctx).min(lines.len());
            let mut parts = Vec::new();
            for (i, l) in lines[lo - 1..hi].iter().enumerate() {
                let n = lo + i;
                if n as u64 == h.line_number {
                    parts.push(format!("> {l}"));
                } else {
                    parts.push(format!("  {l}"));
                }
            }
            rec.state = Some(parts.join("\n"));
        }
        p.total += 1; // the searcher already applied the prefilter
        p.records.push(rec);
    }
}

fn text_records(
    path: &Option<PathBuf>,
    content: &str,
    kind: RecordKind,
    filter: Option<&Prefilter>,
    p: &mut Prepared,
) {
    let body = content.trim_end_matches('\n');
    let n_lines = content.lines().count().max(1) as u64;
    let rec = Record::new(path.clone(), kind, 1, n_lines, body.to_string());
    keep(p, rec, filter);
}

/// Paragraph records: runs of non-blank lines separated by blank lines.
fn para_records(
    path: &Option<PathBuf>,
    content: &str,
    filter: Option<&Prefilter>,
    p: &mut Prepared,
) {
    let lines: Vec<&str> = content.lines().collect();
    let mut i = 0usize;
    while i < lines.len() {
        if lines[i].trim().is_empty() {
            i += 1;
            continue;
        }
        let start = i;
        while i < lines.len() && !lines[i].trim().is_empty() {
            i += 1;
        }
        let body = lines[start..i].join("\n");
        let rec = Record::new(
            path.clone(),
            RecordKind::Para,
            start as u64 + 1,
            i as u64,
            body,
        );
        keep(p, rec, filter);
    }
}

fn chunk_records(
    path: &Option<PathBuf>,
    content: &str,
    size: usize,
    overlap: usize,
    filter: Option<&Prefilter>,
    p: &mut Prepared,
) {
    let lines: Vec<&str> = content.lines().collect();
    if lines.is_empty() {
        return;
    }
    let step = size.saturating_sub(overlap).max(1);
    let mut start = 0usize;
    while start < lines.len() {
        let end = (start + size).min(lines.len());
        let body = lines[start..end].join("\n");
        let mut rec = Record::new(
            path.clone(),
            RecordKind::Chunk,
            start as u64 + 1,
            end as u64,
            body,
        );
        rec.state = Some(rec.body.clone());
        keep(p, rec, filter);
        start += step;
    }
}

fn function_records(
    args: &Args,
    path: &Option<PathBuf>,
    content: &str,
    filter: Option<&Prefilter>,
    p: &mut Prepared,
) {
    let Some(path) = path else { return };
    let Some(lang) = code::lang_for_path(path) else {
        return;
    };
    for f in code::extract_functions(content, lang) {
        p.total += 1;
        if f.body.chars().count() > args.max_chars {
            note(&format!(
                "{}: function too large ({} lines, lines {}-{}); skipping (never truncated)",
                path.display(),
                f.end - f.start + 1,
                f.start,
                f.end
            ));
            p.errors += 1;
            continue;
        }
        let mut rec = Record::new(
            Some(path.clone()),
            RecordKind::Function,
            f.start,
            f.end,
            f.body,
        );
        rec.state = Some(rec.body.clone());
        if let Some(pf) = filter {
            if !pf.is_match(&rec.body) {
                continue;
            }
        }
        p.records.push(rec);
    }
}

/// Diff-mode records: hunks from a git log -p / format-patch / plain diff stream.
fn diff_records(
    args: &Args,
    path: &Option<PathBuf>,
    content: &str,
    repo: Option<PathBuf>,
    filter: Option<&Prefilter>,
    p: &mut Prepared,
) {
    let max_chars = args.max_chars;
    for h in diff::parse_stream(content) {
        p.total += 1;
        let mut rec = Record::new(
            path.clone(),
            RecordKind::DiffHunk,
            h.target_start.max(1) as u64,
            (h.target_start + h.target_length.saturating_sub(1)).max(1) as u64,
            h.body.clone(),
        );
        rec.hunk = Some(HunkMeta {
            commit: h.commit.clone(),
            file: h.file.clone(),
            is_new: h.is_new,
            is_deleted: h.is_deleted,
            source_start: h.source_start,
            source_length: h.source_length,
            target_start: h.target_start,
            target_length: h.target_length,
            section_header: h.section_header.clone(),
            first_target_line: h.first_target_line,
        });
        rec.state = Some(h.body.clone());
        if let Some(pf) = filter {
            if !pf.is_match(&h.body) {
                continue;
            }
        }
        if rec.body.chars().count() > max_chars {
            note(&format!(
                "hunk in {} too large ({} chars, max {max_chars}); skipping (never truncated)",
                h.file,
                rec.body.chars().count()
            ));
            p.errors += 1;
            continue;
        }
        if args.function_context {
            attach_w_context(args, &mut rec, repo.as_deref(), p);
        }
        p.records.push(rec);
    }
}

/// `-W`: attach enclosing-function context from the post-image git blob.
fn attach_w_context(args: &Args, rec: &mut Record, repo: Option<&Path>, p: &mut Prepared) {
    let Some(meta) = rec.hunk.clone() else { return };
    if meta.is_deleted {
        // Deleted files have no post-image blob to read.
        p.ctx_fallbacks
            .entry(gitctx::DELETED_FILE)
            .and_modify(|c| *c += 1)
            .or_insert(1);
        return;
    }
    let Some(repo) = repo else {
        p.ctx_fallbacks
            .entry("no_git_repo")
            .and_modify(|c| *c += 1)
            .or_insert(1);
        return;
    };
    let Some(commit) = &meta.commit else {
        p.ctx_fallbacks
            .entry("no_commit")
            .and_modify(|c| *c += 1)
            .or_insert(1);
        return;
    };
    // First changed line in the post image (where the edit actually is).
    let target_line = meta.first_target_line.max(1);
    match gitctx::enclosing_function(repo, commit, &meta.file, target_line, args.max_chars) {
        Ok(f) => {
            let t0 = meta.target_start.max(1) as u64;
            let t1 = (meta.target_start + meta.target_length.saturating_sub(1)).max(1) as u64;
            if f.start >= t0 && f.end <= t1 {
                // The enclosing function is fully inside the hunk: context adds nothing.
                p.ctx_fallbacks
                    .entry(gitctx::FUNCTION_IN_HUNK)
                    .and_modify(|c| *c += 1)
                    .or_insert(1);
            } else {
                let hunk = rec.body.clone();
                rec.state = Some(format!("{hunk}\n\n{}", f.body));
                rec.with_context = true;
            }
        }
        Err(reason) => {
            p.ctx_fallbacks
                .entry(reason)
                .and_modify(|c| *c += 1)
                .or_insert(1);
        }
    }
}
