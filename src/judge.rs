//! Orchestration: records -> states -> cache -> concurrent batched judging ->
//! ordered streaming decisions -> grep-compatible output.

use crate::cache::{self, Cache};
use crate::cli::{instructions, template_id, Args};
use crate::emit::{self, Emitter};
use crate::inputs::Prepared;
use crate::laya::{Laya, Usage, DEFAULT_API};
use crate::prefilter::{Mode, Prefilter};
use crate::record::{Record, RecordKind};
use crate::stats::{estimate_tokens, Stats};
use anyhow::Result;
use futures::stream::{self, StreamExt};
use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

/// Result of one batch request: answers, or a budget halt.
enum BatchOutcome {
    Done(Vec<Vec<Option<f64>>>, Usage),
    Halted,
}

/// Unique deduped states built from records.
pub struct StateSet {
    pub states: Vec<String>,
    pub templates: Vec<(RecordKind, bool)>,
    /// Per-record state index (None = blank, short-circuits without an API call).
    pub rec_state: Vec<Option<usize>>,
    pub blanks: u64,
}

/// Build unique deduped states from records. Identical records share one
/// state; blank records short-circuit (None) without any API call.
pub fn build_states(records: &[Record], max_chars: usize) -> StateSet {
    let mut set = StateSet {
        states: Vec::new(),
        templates: Vec::new(),
        rec_state: Vec::with_capacity(records.len()),
        blanks: 0,
    };
    let mut seen: HashMap<(String, String), usize> = HashMap::new();
    for rec in records {
        if rec.is_blank() {
            set.rec_state.push(None);
            set.blanks += 1;
            continue;
        }
        let text = state_text(rec, max_chars);
        let tid = template_id(rec.kind, rec.with_context);
        let key = (tid, text.clone());
        let idx = match seen.get(&key) {
            Some(&i) => i,
            None => {
                let i = set.states.len();
                set.states.push(text);
                set.templates.push((rec.kind, rec.with_context));
                seen.insert(key, i);
                i
            }
        };
        set.rec_state.push(Some(idx));
    }
    set
}

fn state_text(rec: &Record, max_chars: usize) -> String {
    let text = rec.state_text();
    match rec.kind {
        // Code units are never silently truncated (size-checked at build time).
        RecordKind::DiffHunk | RecordKind::Function => text.to_string(),
        _ => text.chars().take(max_chars).collect(),
    }
}

fn prefilter_mode(m: crate::cli::PrefilterMode) -> Mode {
    match m {
        crate::cli::PrefilterMode::Keywords => Mode::Keywords,
        crate::cli::PrefilterMode::All => Mode::All,
    }
}

pub async fn run(args: Args) -> Result<i32> {
    let descs = args.all_descriptions();
    let (prefilter, note) = Prefilter::new(prefilter_mode(args.prefilter), &descs)?;
    if let Some(n) = note {
        eprintln!("{n}");
    }

    if args.estimate || args.emit_records {
        return crate::estimate::offline(&args, &prefilter);
    }

    let mut stats = Stats::default();
    stats.start();

    let prepared: Prepared = crate::inputs::prepare(&args, Some(&prefilter))?;
    stats.files = prepared.files_read;
    stats.errors += prepared.errors;
    stats.binary_skipped = prepared.binary_skipped;
    for (k, v) in prepared.ctx_fallbacks {
        *stats.ctx_fallbacks.entry(k).or_insert(0) += v;
    }
    stats.records = prepared.total;
    stats.candidates = prepared.records.len() as u64;
    let records = prepared.records;

    let set = build_states(&records, args.max_chars);
    let states = set.states;
    let templates = set.templates;
    let rec_state = set.rec_state;
    stats.blank_skipped = set.blanks;
    stats.unique_states = states.len() as u64;

    let api_base = args
        .api
        .clone()
        .unwrap_or_else(|| DEFAULT_API.to_string());
    let cache = if args.no_cache { None } else { Cache::open_default() };

    // Per-state instructions (one noul question per description).
    let ins: Vec<Vec<String>> = templates
        .iter()
        .map(|&(k, marked)| {
            descs
                .iter()
                .map(|d| instructions(k, marked, d))
                .collect()
        })
        .collect();

    let mut probs: Vec<Vec<Option<f64>>> = vec![vec![None; descs.len()]; states.len()];
    let mut keys: Vec<Vec<String>> = Vec::with_capacity(states.len());
    for i in 0..states.len() {
        let mut row = Vec::with_capacity(descs.len());
        for j in 0..descs.len() {
            row.push(cache::key(&states[i], &ins[i][j], &api_base, &args.model));
        }
        keys.push(row);
    }
    if let Some(c) = &cache {
        for i in 0..states.len() {
            for j in 0..descs.len() {
                if let Some(p) = c.get(&keys[i][j]) {
                    probs[i][j] = Some(p);
                    stats.cache_hits += 1;
                }
            }
        }
    }

    // Group states by question template, in first-seen order.
    let mut group_order: Vec<String> = Vec::new();
    let mut groups: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, t) in templates.iter().enumerate() {
        let tid = template_id(t.0, t.1);
        if !groups.contains_key(&tid) {
            group_order.push(tid.clone());
        }
        groups.entry(tid).or_default().push(i);
    }

    let laya = Laya::new(&api_base, &args.model, Duration::from_secs(args.timeout))?;
    let spend = std::sync::Arc::new(std::sync::Mutex::new(0u64));
    let multi_group = group_order.len() > 1;
    let mut deferred: Vec<(usize, f64, BTreeMap<String, f64>)> = Vec::new();
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut matched = false;
    let mut broken = false;
    let mut budget_halted = false;

    // Colors: JSON/record output is machine-readable and never colored.
    let painter = if args.json || args.record {
        crate::color::Painter::disabled()
    } else {
        crate::color::Painter::new(args.color)
    };
    let highlight = crate::prefilter::highlight_regex(&descs);

    let mut emitter = Emitter::new(
        args.show_probability,
        args.line_number,
        args.json,
        args.record,
        show_path(&args, &records),
        painter,
        highlight,
    );

    'groups: for tid in &group_order {
        let idxs_all = groups[tid].clone();
        let bs = args.batch_size;
        let chunk_list: Vec<Vec<usize>> = idxs_all.chunks(bs).map(|c| c.to_vec()).collect();
        // Records per chunk, in input order.
        let mut recs_of_chunk: Vec<Vec<usize>> = vec![Vec::new(); chunk_list.len()];
        {
            let mut batch_of_state = HashMap::new();
            for (ci, chunk) in chunk_list.iter().enumerate() {
                for &i in chunk {
                    batch_of_state.insert(i, ci);
                }
            }
            for (ri, rs) in rec_state.iter().enumerate() {
                if let Some(si) = rs {
                    if let Some(&ci) = batch_of_state.get(si) {
                        recs_of_chunk[ci].push(ri);
                    }
                }
            }
        }

        // Precompute which states still need answers (before any mutable borrow).
        let chunk_needs: Vec<(Vec<usize>, Vec<usize>)> = chunk_list
            .iter()
            .map(|idxs| {
                let uncached: Vec<usize> = idxs
                    .iter()
                    .cloned()
                    .filter(|&i| probs[i].iter().any(|p| p.is_none()))
                    .collect();
                (idxs.clone(), uncached)
            })
            .collect();

        let futures = chunk_needs.into_iter().map(|(idxs, uncached)| {
            let laya = &laya;
            let spend = &spend;
            let ins_j: Vec<String> = (0..descs.len()).map(|j| ins[idxs[0]][j].clone()).collect();
            let u_states: Vec<String> = uncached.iter().map(|&i| states[i].clone()).collect();
            let budget = args.budget;
            async move {
                if uncached.is_empty() {
                    return (idxs, uncached, Ok(BatchOutcome::Done(Vec::new(), Usage::default())));
                }
                // Reserve the estimated cost before sending so concurrent
                // in-flight requests cannot overshoot the budget.
                let est: u64 = u_states.iter().map(|s| estimate_tokens(s, &ins_j)).sum();
                {
                    let mut s = spend.lock().unwrap();
                    if budget > 0 && *s + est > budget {
                        return (idxs, uncached, Ok(BatchOutcome::Halted));
                    }
                    *s += est;
                }
                match laya.ask(&u_states, &ins_j, args.concurrency).await {
                    Ok(outcome) => {
                        let probs_out = outcome.probs;
                        let usage = outcome.usage;
                        {
                            let mut s = spend.lock().unwrap();
                            *s = s.saturating_sub(est).saturating_add(usage.input_tokens);
                        }
                        (idxs, uncached, Ok(BatchOutcome::Done(probs_out, usage)))
                    }
                    Err(e) => {
                        // Release the reservation; the request did not happen.
                        {
                            let mut s = spend.lock().unwrap();
                            *s = s.saturating_sub(est);
                        }
                        (idxs, uncached, Err(e))
                    }
                }
            }
        });

        let mut stream = Box::pin(stream::iter(futures).buffered(args.concurrency));
        while let Some((idxs, uncached, res)) = stream.next().await {
            match res {
                Ok(BatchOutcome::Done(probs_out, usage)) => {
                    if !uncached.is_empty() {
                        stats.api_calls += 1;
                        stats.batches += 1;
                        stats.input_tokens += usage.input_tokens;
                        stats.output_tokens += usage.output_tokens;
                        for (k, &si) in uncached.iter().enumerate() {
                            for j in 0..descs.len() {
                                probs[si][j] = probs_out[k][j];
                                if let Some(p) = probs_out[k][j] {
                                    if let Some(c) = &cache {
                                        c.put(&keys[si][j], p);
                                    }
                                }
                            }
                        }
                    }

                    // Emit this chunk's records (in input order).
                    let chunk_idx = chunk_list.iter().position(|c| c == &idxs).unwrap_or(0);
                    for &ri in &recs_of_chunk[chunk_idx] {
                        let rec = &records[ri];
                        let Some(si) = rec_state[ri] else { continue };
                        let row = &probs[si];
                        if row.iter().any(|p| p.is_none()) {
                            stats.errors += 1;
                            continue;
                        }
                        let ps: Vec<f64> = row.iter().map(|p| p.unwrap_or(0.0)).collect();
                        let p = if args.all {
                            ps.iter().cloned().fold(f64::INFINITY, f64::min)
                        } else {
                            ps.iter().cloned().fold(f64::NEG_INFINITY, f64::max)
                        };
                        let is_match = (p >= args.threshold) != args.invert_match;
                        if !is_match {
                            continue;
                        }
                        let path_key = rec.display_path().to_string();
                        if let Some(mc) = args.max_count {
                            if counts.get(&path_key).copied().unwrap_or(0) >= mc {
                                continue;
                            }
                        }
                        *counts.entry(path_key).or_insert(0) += 1;
                        stats.matches += 1;
                        matched = true;
                        if !args.quiet && !args.count && !args.files_with_matches {
                            let answers: BTreeMap<String, f64> = ps
                                .iter()
                                .enumerate()
                                .map(|(j, v)| (format!("d{j}"), *v))
                                .collect();
                            if multi_group {
                                deferred.push((ri, p, answers));
                            } else if !emitter.emit(rec, p, &answers) {
                                broken = true;
                                break;
                            }
                        }
                    }
                    if broken {
                        break 'groups;
                    }
                }
                Ok(BatchOutcome::Halted) => {
                    let spent = *spend.lock().unwrap();
                    eprintln!(
                        "rg1: budget reached ({} input tokens >= {}); halting, remaining records not judged",
                        spent, args.budget
                    );
                    budget_halted = true;
                    break 'groups;
                }
                Err(e) => {
                    eprintln!("rg1: {e}");
                    stats.errors += 1;
                }
            }
            if let Some(mc) = args.max_count {
                let all_saturated = counts
                    .values()
                    .all(|&c| c >= mc)
                    && counts.len() >= files_with_records(&rec_state, &records);
                if all_saturated && !counts.is_empty() {
                    break 'groups;
                }
            }
        }
    }

    // Blank records: p=0.0 deterministically, no API call. Emitted in a final
    // pass (they never reach a batch).
    for (ri, rs) in rec_state.iter().enumerate() {
        if rs.is_some() || broken {
            continue;
        }
        let p = 0.0;
        let is_match = (p >= args.threshold) != args.invert_match;
        if !is_match {
            continue;
        }
        let rec = &records[ri];
        let path_key = rec.display_path().to_string();
        if let Some(mc) = args.max_count {
            if counts.get(&path_key).copied().unwrap_or(0) >= mc {
                continue;
            }
        }
        *counts.entry(path_key).or_insert(0) += 1;
        stats.matches += 1;
        matched = true;
        if !args.quiet && !args.count && !args.files_with_matches {
            let answers: BTreeMap<String, f64> = (0..descs.len())
                .map(|j| (format!("d{j}"), 0.0))
                .collect();
            if multi_group {
                deferred.push((ri, p, answers));
            } else if !emitter.emit(rec, p, &answers) {
                broken = true;
            }
        }
    }

    // Multi-group runs defer emission to preserve input order.
    if multi_group && !broken {
        deferred.sort_by_key(|(ri, _, _)| *ri);
        for (ri, p, answers) in deferred {
            if !emitter.emit(&records[ri], p, &answers) {
                break;
            }
        }
    }

    if (args.count || args.files_with_matches) && !args.quiet {
        let stdin_only = args.paths.is_empty()
            || (args.paths.len() == 1 && args.paths[0].as_os_str() == "-");
        if stdin_only && counts.len() <= 1 {
            let total = counts.values().sum::<usize>();
            emit::emit_total(total, args.files_with_matches);
        } else {
            emit::emit_counts(&counts, args.files_with_matches, &painter_for_counts(&args));
        }
    }

    if args.stats {
        eprintln!("rg1 stats:\n{}", stats.summary());
    }

    if broken {
        return Ok(141); // SIGPIPE-style exit
    }
    if stats.errors > 0 {
        return Ok(2);
    }
    if budget_halted && !matched {
        return Ok(2);
    }
    Ok(if matched { 0 } else { 1 })
}

fn painter_for_counts(args: &Args) -> crate::color::Painter {
    crate::color::Painter::new(args.color)
}

fn show_path(args: &Args, records: &[Record]) -> bool {    if args.no_filename {
        return false;
    }
    let has_stdin = args.paths.is_empty() || args.paths.iter().any(|p| p.as_os_str() == "-");
    if args.with_filename {
        return true;
    }
    if has_stdin {
        return false;
    }
    // Multiple distinct paths -> show filenames (grep convention).
    let mut distinct: Vec<_> = records.iter().map(|r| r.display_path().to_string()).collect();
    distinct.sort();
    distinct.dedup();
    distinct.len() > 1
}

fn files_with_records(rec_state: &[Option<usize>], records: &[Record]) -> usize {
    let mut set = std::collections::BTreeSet::new();
    for (ri, rs) in rec_state.iter().enumerate() {
        if rs.is_some() {
            set.insert(records[ri].display_path().to_string());
        }
    }
    set.len()
}
