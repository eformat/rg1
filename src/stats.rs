//! Run statistics and the input-token budget seat belt.

use std::time::Instant;

#[derive(Default)]
pub struct Stats {
    pub started: Option<Instant>,
    pub files: u64,
    pub records: u64,
    pub candidates: u64,
    pub binary_skipped: u64,
    pub blank_skipped: u64,
    pub unique_states: u64,
    pub batches: u64,
    pub api_calls: u64,
    pub cache_hits: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub errors: u64,
    pub ctx_fallbacks: std::collections::BTreeMap<&'static str, u64>,
    pub matches: u64,
}

impl Stats {
    pub fn start(&mut self) {
        self.started = Some(Instant::now());
    }

    pub fn elapsed(&self) -> Option<std::time::Duration> {
        self.started.map(|s| s.elapsed())
    }

    pub fn summary(&self) -> String {
        let mut lines = Vec::new();
        let elapsed = self
            .elapsed()
            .map(|d| format!("{:.2}s", d.as_secs_f64()))
            .unwrap_or_else(|| "-".to_string());
        lines.push(format!(
            "files: {}  records: {}  candidates: {}  unique states: {}",
            self.files, self.records, self.candidates, self.unique_states
        ));
        lines.push(format!(
            "batches: {}  api calls: {}  cache hits: {}  matches: {}  errors: {}",
            self.batches, self.api_calls, self.cache_hits, self.matches, self.errors
        ));
        lines.push(format!(
            "tokens: {} in / {} out  elapsed: {elapsed}",
            self.input_tokens, self.output_tokens
        ));
        if self.binary_skipped > 0 {
            lines.push(format!("binary files skipped: {}", self.binary_skipped));
        }
        if self.blank_skipped > 0 {
            lines.push(format!(
                "blank records short-circuited: {}",
                self.blank_skipped
            ));
        }
        if !self.ctx_fallbacks.is_empty() {
            let parts: Vec<String> = self
                .ctx_fallbacks
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect();
            lines.push(format!("-W fallbacks: {}", parts.join(", ")));
        }
        lines.join("\n")
    }
}

/// Rough pre-flight token estimate for a batch (used for the budget reserve).
pub fn estimate_tokens(state: &str, instructions: &[String]) -> u64 {
    let mut t = (state.chars().count() as u64) / 4 + 1;
    for ins in instructions {
        t += (ins.chars().count() as u64) / 4 + 12;
    }
    t
}
