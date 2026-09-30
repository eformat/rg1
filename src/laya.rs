//! Client for the laya-studio decision engine ("decision 1" / systemone API).
//!
//! One forward pass per record: text `state` in, typed `noul` (yes/no
//! probability) answers out. Batches up to 1024 states per request; falls back
//! to the single endpoint if the batch endpoint is missing; retries 5xx/503
//! with backoff honoring `Retry-After`.

use anyhow::{anyhow, bail, Result};
use serde_json::json;
use std::time::Duration;

pub const DEFAULT_API: &str = "https://laya-studio-user-mhepburn.apps.ocp.cloud.rhai-tmm.dev";
pub const MAX_STATES_PER_REQUEST: usize = 1024;
pub const MAX_QUESTIONS: usize = 64;

pub struct Laya {
    base: String,
    model: String,
    http: reqwest::Client,
    use_single: std::sync::atomic::AtomicBool,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

pub struct AskOutcome {
    /// probabilities[state_index][question_index]
    pub probs: Vec<Vec<Option<f64>>>,
    pub usage: Usage,
}

impl Laya {
    pub fn new(base: &str, model: &str, timeout: Duration) -> reqwest::Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(timeout)
            .user_agent(concat!("rg1/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Laya {
            base: base.trim_end_matches('/').to_string(),
            model: model.to_string(),
            http,
            use_single: std::sync::atomic::AtomicBool::new(false),
        })
    }

    /// Judge `states` against `instructions` (one noul question per description).
    /// Returns probabilities aligned to the input states.
    pub async fn ask(
        &self,
        states: &[String],
        instructions: &[String],
        concurrency: usize,
    ) -> Result<AskOutcome> {
        if states.is_empty() {
            return Ok(AskOutcome {
                probs: Vec::new(),
                usage: Usage::default(),
            });
        }
        if instructions.len() > MAX_QUESTIONS {
            bail!(
                "too many descriptions ({}, max {MAX_QUESTIONS})",
                instructions.len()
            );
        }
        let questions: serde_json::Map<String, serde_json::Value> = instructions
            .iter()
            .enumerate()
            .map(|(i, ins)| {
                (
                    format!("d{i}"),
                    json!({"type": "noul", "instructions": ins}),
                )
            })
            .collect();
        let questions = serde_json::Value::Object(questions);

        if self.use_single.load(std::sync::atomic::Ordering::Relaxed) {
            return self.ask_single(states, &questions, concurrency).await;
        }
        match self.ask_batch(states, &questions).await {
            Ok(outcome) => Ok(outcome),
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("404") {
                    // Deployment without the batch endpoint: fall back permanently.
                    self.use_single
                        .store(true, std::sync::atomic::Ordering::Relaxed);
                    self.ask_single(states, &questions, concurrency).await
                } else {
                    Err(e)
                }
            }
        }
    }

    async fn ask_batch(
        &self,
        states: &[String],
        questions: &serde_json::Value,
    ) -> Result<AskOutcome> {
        let url = format!("{}/v1/systemone/batches", self.base);
        let body = json!({
            "model": self.model,
            "states": states.iter().enumerate()
                .map(|(i, s)| json!({"id": format!("s{i}"), "state": s}))
                .collect::<Vec<_>>(),
            "questions": questions,
        });
        let resp = self.send(&url, &body).await?;
        let value: serde_json::Value = serde_json::from_str(&resp)
            .map_err(|e| anyhow!("invalid JSON from batch endpoint: {e}"))?;
        parse_batch_response(&value, states.len(), instructions_count(questions))
    }

    async fn ask_single(
        &self,
        states: &[String],
        questions: &serde_json::Value,
        concurrency: usize,
    ) -> Result<AskOutcome> {
        use futures::stream::{self, StreamExt};
        let n_q = instructions_count(questions);
        let url = format!("{}/v1/systemone", self.base);
        let sem = std::sync::Arc::new(tokio::sync::Semaphore::new(concurrency.max(1)));
        let futures = states.iter().enumerate().map(|(i, state)| {
            let url = url.clone();
            let body = json!({
                "model": self.model,
                "state": state,
                "questions": questions,
            });
            let sem = sem.clone();
            async move {
                let _permit = sem.acquire_owned().await;
                let resp = self.send(&url, &body).await?;
                let value: serde_json::Value =
                    serde_json::from_str(&resp).map_err(|e| anyhow!("invalid JSON: {e}"))?;
                parse_single_response(&value, n_q).map(|probs| (i, probs))
            }
        });
        let mut probs = vec![vec![None; n_q]; states.len()];
        let usage = Usage::default();
        let mut first_err: Option<anyhow::Error> = None;
        let mut results = stream::iter(futures).buffered(concurrency.clamp(1, 32));
        while let Some(res) = results.next().await {
            match res {
                Ok((i, p)) => probs[i] = p,
                Err(e) => {
                    if first_err.is_none() {
                        first_err = Some(e);
                    }
                }
            }
        }
        if let Some(e) = first_err {
            return Err(e);
        }
        // Single endpoint usage is not accumulated per-state here; batch mode
        // is the primary path and reports usage.
        Ok(AskOutcome { probs, usage })
    }

    /// POST with retries: 5xx and network errors back off; 4xx is fatal.
    async fn send(&self, url: &str, body: &serde_json::Value) -> Result<String> {
        const MAX_ATTEMPTS: usize = 4;
        let mut attempt = 0;
        loop {
            attempt += 1;
            let result = self.http.post(url).json(body).send().await;
            match result {
                Ok(resp) => {
                    let status = resp.status();
                    if status.is_server_error() {
                        let retry_after = resp
                            .headers()
                            .get("retry-after")
                            .and_then(|v| v.to_str().ok())
                            .and_then(|v| v.parse::<u64>().ok())
                            .unwrap_or(0);
                        if attempt >= MAX_ATTEMPTS {
                            bail!("laya error: {status} after {MAX_ATTEMPTS} attempts ({url})");
                        }
                        let backoff = if retry_after > 0 {
                            Duration::from_secs(retry_after.min(5))
                        } else {
                            Duration::from_millis(250 * (1 << (attempt - 1)).min(8))
                        };
                        tokio::time::sleep(backoff).await;
                        continue;
                    }
                    if status.is_client_error() {
                        let text = resp.text().await.unwrap_or_default();
                        bail!("laya error: {status} {text} ({url})");
                    }
                    return resp.text().await.map_err(|e| anyhow!("read error: {e}"));
                }
                Err(e) => {
                    if e.is_timeout() || e.is_connect() || e.is_request() {
                        if attempt >= MAX_ATTEMPTS {
                            bail!("laya error: {e} after {MAX_ATTEMPTS} attempts ({url})");
                        }
                        tokio::time::sleep(Duration::from_millis(
                            250 * (1 << (attempt - 1)).min(8),
                        ))
                        .await;
                        continue;
                    }
                    bail!("laya error: {e} ({url})");
                }
            }
        }
    }
}

fn instructions_count(questions: &serde_json::Value) -> usize {
    questions.as_object().map(|m| m.len()).unwrap_or(0)
}

fn parse_batch_response(
    value: &serde_json::Value,
    n_states: usize,
    n_questions: usize,
) -> Result<AskOutcome> {
    let results = value
        .get("results")
        .and_then(|r| r.as_array())
        .ok_or_else(|| anyhow!("batch response missing 'results' array"))?;
    if results.len() != n_states {
        bail!(
            "batch response has {} results, expected {n_states}",
            results.len()
        );
    }
    let mut probs = vec![vec![None; n_questions]; n_states];
    let mut saw_id = vec![false; n_states];
    for (i, r) in results.iter().enumerate() {
        // Results are ordered by id "s{i}" per the API contract, but verify.
        let id = r.get("id").and_then(|v| v.as_str()).unwrap_or("");
        let idx = id
            .strip_prefix('s')
            .and_then(|n| n.parse::<usize>().ok())
            .unwrap_or(i);
        if idx < n_states {
            saw_id[idx] = true;
            probs[idx] = parse_answers(r.get("answers"), n_questions);
        }
    }
    if saw_id.iter().any(|s| !s) {
        bail!("batch response missing results for some states");
    }
    let usage = parse_usage(value.get("usage"));
    Ok(AskOutcome { probs, usage })
}

fn parse_single_response(
    value: &serde_json::Value,
    n_questions: usize,
) -> Result<Vec<Option<f64>>> {
    let answers = value
        .get("answers")
        .ok_or_else(|| anyhow!("response missing 'answers'"))?;
    Ok(parse_answers(Some(answers), n_questions))
}

fn parse_answers(answers: Option<&serde_json::Value>, n_questions: usize) -> Vec<Option<f64>> {
    let mut out = vec![None; n_questions];
    if let Some(map) = answers.and_then(|a| a.as_object()) {
        for (qid, v) in map {
            if let Some(idx) = qid.strip_prefix('d').and_then(|n| n.parse::<usize>().ok()) {
                if idx < n_questions {
                    out[idx] = v.get("noul").and_then(|n| n.as_f64());
                }
            }
        }
    }
    out
}

fn parse_usage(u: Option<&serde_json::Value>) -> Usage {
    match u {
        Some(v) => Usage {
            input_tokens: v.get("input_tokens").and_then(|x| x.as_u64()).unwrap_or(0),
            output_tokens: v.get("output_tokens").and_then(|x| x.as_u64()).unwrap_or(0),
        },
        None => Usage::default(),
    }
}
