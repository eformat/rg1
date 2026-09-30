//! grep-compatible output: plain, `-o` probability column, `--json`, counts,
//! filenames. Results stream in input order.

use crate::record::Record;
use std::collections::BTreeMap;
use std::io::{BufWriter, Write};

pub struct Emitter {
    out: BufWriter<std::io::Stdout>,
    show_path: bool,
    show_line: bool,
    show_probability: bool,
    json: bool,
    record: bool,
}

impl Emitter {
    pub fn new(
        show_probability: bool,
        show_line: bool,
        json: bool,
        record: bool,
        show_path: bool,
    ) -> Self {
        Emitter {
            out: BufWriter::new(std::io::stdout()),
            show_path,
            show_line,
            show_probability,
            json,
            record,
        }
    }

    /// Emit one selected (matched) record. Returns false on write error
    /// (e.g. closed pipe) so the caller can stop streaming.
    pub fn emit(&mut self, rec: &Record, p: f64, answers: &BTreeMap<String, f64>) -> bool {
        let result = if self.json || self.record {
            self.emit_json(rec, p, answers)
        } else if self.show_probability {
            self.emit_plain(rec, Some(p))
        } else {
            self.emit_plain(rec, None)
        };
        match result {
            Ok(()) => self.out.flush().is_ok(),
            Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => false,
            Err(_) => false,
        }
    }

    fn emit_plain(&mut self, rec: &Record, p: Option<f64>) -> std::io::Result<()> {
        let prefix = self.prefix(rec);
        let body = &rec.body;
        let mut lines = body.split('\n');
        let first = lines.next().unwrap_or("");
        match p {
            Some(p) => write!(self.out, "{prefix}{p:.3}\t{first}")?,
            None => write!(self.out, "{prefix}{first}")?,
        }
        for l in lines {
            write!(self.out, "\n{l}")?;
        }
        writeln!(self.out)
    }

    fn emit_json(&mut self, rec: &Record, p: f64, answers: &BTreeMap<String, f64>) -> std::io::Result<()> {
        use serde_json::json;
        let path = rec.path.as_ref().map(|p| p.display().to_string());
        if self.record {
            let hunk = rec.hunk.as_ref().map(|h| {
                json!({
                    "commit": h.commit,
                    "file": h.file,
                    "is_new": h.is_new,
                    "is_deleted": h.is_deleted,
                    "source_start": h.source_start,
                    "source_length": h.source_length,
                    "target_start": h.target_start,
                    "target_length": h.target_length,
                    "section_header": h.section_header,
                })
            });
            let obj = json!({
                "record": {
                    "path": path,
                    "kind": rec.kind.as_str(),
                    "start_line": rec.start_line,
                    "end_line": rec.end_line,
                    "with_context": rec.with_context,
                    "hunk": hunk,
                },
                "answers": answers,
                "p": p,
                "match": true,
                "body": rec.body,
            });
            writeln!(self.out, "{obj}")
        } else {
            let obj = json!({
                "path": path,
                "start_line": rec.start_line,
                "end_line": rec.end_line,
                "kind": rec.kind.as_str(),
                "p": p,
                "answers": answers,
                "body": rec.body,
            });
            writeln!(self.out, "{obj}")
        }
    }

    fn prefix(&self, rec: &Record) -> String {
        let mut s = String::new();
        if self.show_path {
            s.push_str(&rec.display_path());
            s.push(':');
        }
        if self.show_line {
            s.push_str(&rec.start_line.to_string());
            s.push(':');
        }
        s
    }
}

/// Print per-file match counts (`-c`) or files with matches (`-l`).
pub fn emit_counts(counts: &BTreeMap<String, usize>, with_files: bool) {
    let out = std::io::stdout();
    let mut out = out.lock();
    for (path, count) in counts {
        if with_files {
            let _ = writeln!(out, "{path}");
        } else {
            let _ = writeln!(out, "{path}:{count}");
        }
    }
}

/// Print a bare count (stdin mode).
pub fn emit_total(count: usize, with_files: bool) {
    if !with_files {
        let _ = writeln!(std::io::stdout().lock(), "{count}");
    }
}
