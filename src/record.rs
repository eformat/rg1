//! The unit of text that gets judged.

use std::borrow::Cow;
use std::path::PathBuf;

/// What kind of input unit a record is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RecordKind {
    Line,
    Para,
    Whole,
    Chunk,
    Jsonl,
    Csv,
    DiffHunk,
    Function,
}

impl RecordKind {
    pub fn as_str(self) -> &'static str {
        match self {
            RecordKind::Line => "line",
            RecordKind::Para => "para",
            RecordKind::Whole => "whole",
            RecordKind::Chunk => "chunk",
            RecordKind::Jsonl => "jsonl",
            RecordKind::Csv => "csv",
            RecordKind::DiffHunk => "diff-hunk",
            RecordKind::Function => "function",
        }
    }
}

/// Metadata for a unified-diff hunk record.
#[derive(Clone, Debug)]
pub struct HunkMeta {
    pub commit: Option<String>,
    /// Post-image (target) file path inside the diff.
    pub file: String,
    pub is_new: bool,
    pub is_deleted: bool,
    pub source_start: usize,
    pub source_length: usize,
    pub target_start: usize,
    pub target_length: usize,
    pub section_header: String,
    /// Post-image line of the first changed (`+`) line.
    pub first_target_line: u64,
}

/// One unit of text to judge.
#[derive(Clone, Debug)]
pub struct Record {
    /// Source path (`None` for stdin).
    pub path: Option<PathBuf>,
    pub kind: RecordKind,
    /// 1-based start line of the record in its source.
    pub start_line: u64,
    /// 1-based end line (>= start_line).
    pub end_line: u64,
    /// The record text as shown in output.
    pub body: String,
    /// Prebuilt state text (e.g. a `-C` context window, or a `-W` diff context);
    /// when `None` the judged text is `body`.
    pub state: Option<String>,
    pub hunk: Option<HunkMeta>,
    /// `-W` context was attached to `state` (diff mode).
    pub with_context: bool,
}

impl Record {
    pub fn new(
        path: Option<PathBuf>,
        kind: RecordKind,
        start_line: u64,
        end_line: u64,
        body: String,
    ) -> Self {
        Record {
            path,
            kind,
            start_line,
            end_line,
            body,
            state: None,
            hunk: None,
            with_context: false,
        }
    }

    pub fn display_path(&self) -> Cow<'_, str> {
        // Diff hunks display their post-image file, not the diff stream.
        if self.kind == RecordKind::DiffHunk {
            if let Some(h) = &self.hunk {
                return Cow::Owned(h.file.clone());
            }
        }
        match &self.path {
            Some(p) => p.to_string_lossy(),
            None => Cow::Borrowed("(stdin)"),
        }
    }

    /// The text the decision engine judges.
    pub fn state_text(&self) -> &str {
        self.state.as_deref().unwrap_or(&self.body)
    }

    /// Whether the judged text is blank (short-circuits to p=0.0, no API call).
    pub fn is_blank(&self) -> bool {
        self.state_text().trim().is_empty()
    }
}
