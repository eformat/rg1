//! Candidate line selection using ripgrep's engine crates (`grep-regex` +
//! `grep-searcher`) — the same search core as `rg`, linked in-process.

use anyhow::Result;
use grep_regex::RegexMatcherBuilder;
use grep_searcher::{BinaryDetection, Searcher, SearcherBuilder, Sink, SinkMatch};

pub struct LineHit {
    pub line_number: u64,
    pub line: String,
}

/// Search an in-memory file for candidate lines.
pub fn search_lines(content: &[u8], pattern: &str, case_insensitive: bool) -> Result<Vec<LineHit>> {
    let matcher = RegexMatcherBuilder::new()
        .case_insensitive(case_insensitive)
        .build(pattern)?;
    let mut searcher = SearcherBuilder::new()
        .line_number(true)
        .binary_detection(BinaryDetection::quit(b'\x00'))
        .build();
    let mut hits = Vec::new();
    searcher.search_slice(&matcher, content, HitSink { hits: &mut hits })?;
    Ok(hits)
}

struct HitSink<'a> {
    hits: &'a mut Vec<LineHit>,
}

impl Sink for HitSink<'_> {
    type Error = std::io::Error;

    fn matched(&mut self, _searcher: &Searcher, mat: &SinkMatch<'_>) -> Result<bool, Self::Error> {
        let line = String::from_utf8_lossy(mat.bytes());
        self.hits.push(LineHit {
            line_number: mat.line_number().unwrap_or(0),
            line: line
                .trim_end_matches('\n')
                .trim_end_matches('\r')
                .to_string(),
        });
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_candidate_lines() {
        let content = b"def login():\n    pass\nuser = login()\n";
        let hits = search_lines(content, r"\b(?:login|user)\b", true).unwrap();
        assert_eq!(
            hits.iter().map(|h| h.line_number).collect::<Vec<_>>(),
            vec![1, 3]
        );
        assert_eq!(hits[0].line, "def login():");
    }

    #[test]
    fn match_all_pattern_hits_every_line() {
        let content = b"a\n\nb\n";
        let hits = search_lines(content, "^", false).unwrap();
        assert_eq!(hits.len(), 3);
    }
}
