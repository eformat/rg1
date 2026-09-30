//! Offline modes: `--estimate` (plan preview without API calls) and
//! `--emit-records` (JSONL dump of the discovered records).

use crate::cache::{self, Cache};
use crate::cli::Args;
use crate::inputs;
use crate::judge::build_states;
use crate::laya::DEFAULT_API;
use crate::prefilter::Prefilter;
use crate::stats::estimate_tokens;
use anyhow::Result;
use sha2::{Digest, Sha256};

pub fn offline(args: &Args, prefilter: &Prefilter) -> Result<i32> {
    let prepared = inputs::prepare(args, Some(prefilter))?;
    let records = &prepared.records;

    if args.emit_records {
        use std::io::Write;
        let out = std::io::stdout();
        let mut w = out.lock();
        for rec in records {
            let obj = serde_json::json!({
                "id": record_id(rec),
                "path": rec.path.as_ref().map(|p| p.display().to_string()),
                "kind": rec.kind.as_str(),
                "start_line": rec.start_line,
                "end_line": rec.end_line,
                "with_context": rec.with_context,
                "body": rec.body,
            });
            writeln!(w, "{obj}")?;
        }
        w.flush()?;
        return Ok(0);
    }

    // --estimate
    let descs = args.all_descriptions();
    let set = build_states(records, args.max_chars);
    let states = &set.states;
    let templates = &set.templates;
    let blanks = set.blanks;
    let ins: Vec<Vec<String>> = templates
        .iter()
        .map(|&(k, marked)| {
            descs
                .iter()
                .map(|d| crate::cli::instructions(k, marked, d))
                .collect()
        })
        .collect();

    let api_base = args.api.clone().unwrap_or_else(|| DEFAULT_API.to_string());
    let cache = if args.no_cache {
        None
    } else {
        Cache::open_default()
    };

    let mut cached = 0u64;
    let mut est: u64 = 0;
    let mut uncached_states = 0u64;
    for i in 0..states.len() {
        est += estimate_tokens(&states[i], &ins[i]);
        let mut any_uncached = false;
        for j in 0..descs.len() {
            let k = cache::key(&states[i], &ins[i][j], &api_base, &args.model);
            match cache.as_ref().and_then(|c| c.get(&k)) {
                Some(_) => cached += 1,
                None => any_uncached = true,
            }
        }
        if any_uncached {
            uncached_states += 1;
        }
    }
    let batches = if args.batch_size > 0 {
        uncached_states.div_ceil(args.batch_size as u64)
    } else {
        0
    };

    let mode = args.input_mode();
    let mode_str = match mode {
        crate::cli::InputMode::Lines => "lines".to_string(),
        crate::cli::InputMode::Para => "para".to_string(),
        crate::cli::InputMode::Whole => "whole".to_string(),
        crate::cli::InputMode::Chunks { size, overlap } => {
            format!("chunks({size}, overlap {overlap})")
        }
        crate::cli::InputMode::Jsonl => "jsonl".to_string(),
        crate::cli::InputMode::Csv => "csv".to_string(),
        crate::cli::InputMode::Diff => "diff".to_string(),
        crate::cli::InputMode::Functions => "functions".to_string(),
    };

    println!("mode: {mode_str}  prefilter: {}", prefilter.describe());
    println!(
        "files: {}  records: {}  candidates: {}  blank: {blanks}",
        prepared.files_read,
        prepared.total,
        records.len()
    );
    println!(
        "unique states: {}  batches: {batches} (batch-size {})",
        states.len(),
        args.batch_size
    );
    println!(
        "est. input tokens: ~{est}  cached answers: {cached}  (model: {})",
        args.model
    );
    if prepared.errors > 0 {
        eprintln!("rg1: {} file(s) could not be read", prepared.errors);
    }
    Ok(0)
}

/// SHA-256-anchored record id (first 16 hex chars).
pub fn record_id(rec: &crate::record::Record) -> String {
    let mut h = Sha256::new();
    h.update(rec.display_path().as_bytes());
    h.update([b'|']);
    h.update(rec.start_line.to_string().as_bytes());
    h.update([b'|']);
    h.update(rec.end_line.to_string().as_bytes());
    h.update([b'|']);
    h.update(rec.body.as_bytes());
    let d = h.finalize();
    d[..8].iter().map(|b| format!("{b:02x}")).collect()
}
