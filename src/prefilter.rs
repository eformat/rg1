//! Description -> candidate strategy.
//!
//! `--prefilter=keywords` derives a case-insensitive keyword alternation regex
//! from the description(s); lines/records that match it become candidates for
//! the decision engine. `--prefilter=all` judges every record.

use anyhow::Result;
use regex::{Regex, RegexBuilder};
use std::collections::BTreeSet;

pub const KEYWORD_LIMIT: usize = 64;

/// Pattern that matches every line (used by the ripgrep engine in `all` mode).
pub const MATCH_ALL: &str = "^";

const STOPWORDS: &[&str] = &[
    "a", "about", "after", "again", "against", "all", "also", "am", "an", "and", "any", "are",
    "as", "at", "be", "because", "been", "before", "being", "below", "between", "both", "but",
    "by", "can", "did", "do", "does", "doing", "down", "during", "each", "either", "few", "for",
    "from", "further", "had", "has", "have", "having", "he", "her", "here", "hers", "him", "his",
    "how", "i", "if", "in", "into", "is", "it", "its", "itself", "just", "me", "more", "most",
    "my", "neither", "no", "nor", "not", "now", "of", "off", "on", "once", "only", "or", "other",
    "our", "ours", "out", "over", "own", "same", "she", "should", "so", "some", "such", "than",
    "that", "the", "their", "theirs", "them", "then", "there", "these", "they", "this", "those",
    "through", "to", "too", "under", "until", "up", "very", "was", "we", "were", "what", "when",
    "where", "which", "while", "who", "whom", "why", "will", "with", "you", "your", "yours",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Keywords,
    All,
}

#[derive(Clone, Debug)]
pub struct Prefilter {
    pub mode: Mode,
    /// Pattern handed to ripgrep's engine for line search ("^" in `all` mode).
    pattern: String,
    /// Regex for record-body matching (None in `all` mode = always a candidate).
    regex: Option<Regex>,
    pub keywords: Vec<String>,
}

impl Prefilter {
    /// Build a prefilter from descriptions. Returns the prefilter plus an
    /// optional note to print on stderr (`keywords` with no extractable words
    /// degrades to `all`).
    pub fn new(mode: Mode, descriptions: &[String]) -> Result<(Self, Option<String>)> {
        match mode {
            Mode::All => Ok((
                Self {
                    mode,
                    pattern: MATCH_ALL.to_string(),
                    regex: None,
                    keywords: Vec::new(),
                },
                None,
            )),
            Mode::Keywords => {
                let words = keywords(descriptions);
                if words.is_empty() {
                    let note = "rg1: no keywords found in description(s); \
                                falling back to --prefilter=all"
                        .to_string();
                    return Ok((
                        Self {
                            mode: Mode::All,
                            pattern: MATCH_ALL.to_string(),
                            regex: None,
                            keywords: Vec::new(),
                        },
                        Some(note),
                    ));
                }
                let alternation = words.iter().map(|w| regex::escape(w)).collect::<Vec<_>>().join("|");
                // Separators, not \b: `_` counts as a separator so keywords match
                // inside identifiers like Handle_User_Login (recall-first: the
                // decision engine provides precision).
                let pattern = format!(r"(?:^|[^0-9A-Za-z])(?:{alternation})(?:[^0-9A-Za-z]|$)");
                let re = RegexBuilder::new(&pattern)
                    .case_insensitive(true)
                    .size_limit(4 << 20)
                    .build()?;
                Ok((
                    Self {
                        mode,
                        pattern,
                        regex: Some(re),
                        keywords: words,
                    },
                    None,
                ))
            }
        }
    }

    /// Pattern for grep-regex line search.
    pub fn line_pattern(&self) -> &str {
        &self.pattern
    }

    /// Whether a record body becomes a candidate for judging.
    pub fn is_match(&self, text: &str) -> bool {
        match &self.regex {
            None => true,
            Some(re) => re.is_match(text),
        }
    }

    pub fn describe(&self) -> String {
        match self.mode {
            Mode::All => "all".to_string(),
            Mode::Keywords => format!("keywords({} words)", self.keywords.len()),
        }
    }
}

/// Extract candidate keywords from descriptions: alphanumeric tokens, lowercased,
/// stopwords and 1-char tokens dropped, deduped, deterministically capped.
pub fn keywords(descriptions: &[String]) -> Vec<String> {
    let mut seen = BTreeSet::new();
    for d in descriptions {
        for tok in d.split(|c: char| !c.is_alphanumeric()) {
            let t = tok.to_lowercase();
            if t.chars().count() >= 2 && !STOPWORDS.contains(&t.as_str()) {
                seen.insert(t);
            }
        }
    }
    let mut v: Vec<String> = seen.into_iter().collect();
    v.truncate(KEYWORD_LIMIT);
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_meaningful_words() {
        let ks = keywords(&["python code that prints a greeting".to_string()].to_vec());
        assert_eq!(ks, vec!["code", "greeting", "prints", "python"]);
    }

    #[test]
    fn empty_after_stopwords_degrades() {
        let (p, note) = Prefilter::new(Mode::Keywords, &["the of and".to_string()].to_vec())
            .unwrap();
        assert_eq!(p.mode, Mode::All);
        assert!(note.is_some());
        assert!(p.is_match("anything"));
        assert_eq!(p.line_pattern(), MATCH_ALL);
    }

    #[test]
    fn keywords_match_case_insensitively() {
        let (p, _) = Prefilter::new(
            Mode::Keywords,
            &["handle user login".to_string()].to_vec(),
        )
        .unwrap();
        assert!(p.is_match("def Handle_User_Login():"));
        assert!(!p.is_match("total = a + b"));
        assert!(p.line_pattern().contains("login"));
    }
}
