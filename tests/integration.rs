//! Integration tests: a mock laya-studio server (wiremock) + the compiled
//! `rg1` binary, exercising batching, caching, retries, budget, and output.

use serde_json::{json, Value};
use std::io::Write;
use std::process::{Command, Stdio};
use tempfile::TempDir;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

/// A stand-in for the laya-studio decision engine: returns noul = p_match for
/// states containing the keyword, p_other otherwise, for every question.
/// Responds in batch shape for `states` requests and single shape for `state`.
struct DecideLaya {
    keyword: &'static str,
    p_match: f64,
    p_other: f64,
}

impl DecideLaya {
    fn answers(&self, text: &str, n_q: usize) -> serde_json::Map<String, Value> {
        let p = if text.contains(self.keyword) {
            self.p_match
        } else {
            self.p_other
        };
        let mut answers = serde_json::Map::new();
        for i in 0..n_q {
            answers.insert(format!("d{i}"), json!({"type": "noul", "noul": p}));
        }
        answers
    }
}

impl Respond for DecideLaya {
    fn respond(&self, req: &Request) -> ResponseTemplate {
        let body: Value = req.body_json().unwrap_or(Value::Null);
        let n_q = body
            .get("questions")
            .and_then(|q| q.as_object())
            .map(|m| m.len())
            .unwrap_or(1);
        let usage = json!({"input_tokens": 100, "output_tokens": 0});
        if let Some(states) = body.get("states").and_then(|s| s.as_array()) {
            let mut results = Vec::new();
            for s in states {
                let id = s
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let text = s.get("state").and_then(|v| v.as_str()).unwrap_or("");
                results.push(json!({"id": id, "answers": self.answers(text, n_q)}));
            }
            ResponseTemplate::new(200).set_body_json(json!({
                "model": "test-model",
                "results": results,
                "usage": usage,
            }))
        } else {
            let text = body.get("state").and_then(|v| v.as_str()).unwrap_or("");
            ResponseTemplate::new(200).set_body_json(json!({
                "model": "test-model",
                "answers": self.answers(text, n_q),
                "usage": usage,
            }))
        }
    }
}

struct Run {
    stdout: String,
    stderr: String,
    code: i32,
}

fn run(args: &[&str], env: &[(&str, std::path::PathBuf)], stdin: Option<&str>) -> Run {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_rg1"));
    cmd.args(args);
    for (k, v) in env {
        cmd.env(k, v);
    }
    if let Some(s) = stdin {
        // Feed stdin from a temp file (deterministic; no pipe coordination).
        let mut f = tempfile::NamedTempFile::new().expect("tempfile");
        f.write_all(s.as_bytes()).expect("write stdin fixture");
        f.flush().expect("flush stdin fixture");
        cmd.stdin(Stdio::from(f.reopen().expect("reopen")));
        let out = cmd.output().expect("spawn rg1");
        return Run {
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            code: out.status.code().unwrap_or(-1),
        };
    }
    let out = cmd.output().expect("spawn rg1");
    Run {
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        code: out.status.code().unwrap_or(-1),
    }
}

fn mock_server(rt: &tokio::runtime::Runtime, keyword: &'static str) -> MockServer {
    rt.block_on(async {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/systemone/batches"))
            .respond_with(DecideLaya {
                keyword,
                p_match: 0.9,
                p_other: 0.1,
            })
            .mount(&server)
            .await;
        server
    })
}

fn fresh_cache() -> (TempDir, Vec<(&'static str, std::path::PathBuf)>) {
    let dir = tempfile::tempdir().unwrap();
    let env = vec![("XDG_CACHE_HOME", dir.path().to_path_buf())];
    (dir, env)
}

#[test]
fn keywords_prefilter_finds_and_exits_zero() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let server = mock_server(&rt, "needle");
    let (_c, cache_env) = fresh_cache();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("a.txt"),
        "has the needle here\nnothing to see\n",
    )
    .unwrap();
    let r = run(
        &[
            "needle in the haystack",
            "--prefilter",
            "keywords",
            "--api",
            &server.uri(),
            dir.path().to_str().unwrap(),
        ],
        &cache_env,
        None,
    );
    assert_eq!(r.code, 0, "stderr: {}", r.stderr);
    assert!(
        r.stdout.contains("has the needle here"),
        "stdout: {}",
        r.stdout
    );
    assert!(!r.stdout.contains("nothing to see"));
}

#[test]
fn prefilter_defaults_to_keywords() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let server = mock_server(&rt, "needle");
    let (_c, cache_env) = fresh_cache();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("a.txt"),
        "has the needle here\nnothing to see\n",
    )
    .unwrap();
    // No --prefilter flag: defaults to keywords (only the needle line judged).
    let r = run(
        &[
            "needle in the haystack",
            "--api",
            &server.uri(),
            dir.path().to_str().unwrap(),
        ],
        &cache_env,
        None,
    );
    assert_eq!(r.code, 0, "stderr: {}", r.stderr);
    assert!(
        r.stdout.contains("has the needle here"),
        "stdout: {}",
        r.stdout
    );
    assert!(!r.stdout.contains("nothing to see"));
    let received = rt
        .block_on(async { server.received_requests().await })
        .unwrap();
    assert_eq!(received.len(), 1);
    let body: Value = serde_json::from_slice(&received[0].body).unwrap();
    let states = body["states"].as_array().unwrap();
    assert_eq!(
        states.len(),
        1,
        "default prefilter=keywords: only the needle line is a candidate"
    );
    assert!(states[0]["state"].as_str().unwrap().contains("needle"));
}

#[test]
fn no_match_exits_one() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let server = mock_server(&rt, "needle");
    let (_c, cache_env) = fresh_cache();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "alpha\nbeta\n").unwrap();
    let r = run(
        &[
            "quixotic zendoodle",
            "--prefilter",
            "all",
            "--api",
            &server.uri(),
            dir.path().to_str().unwrap(),
        ],
        &cache_env,
        None,
    );
    assert_eq!(r.code, 1, "stderr: {}", r.stderr);
    assert!(r.stdout.is_empty(), "stdout: {}", r.stdout);
}

#[test]
fn invert_match_selects_non_matches() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let server = mock_server(&rt, "needle");
    let (_c, cache_env) = fresh_cache();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("a.txt"),
        "has the needle here\nnothing to see\n",
    )
    .unwrap();
    let r = run(
        &[
            "needle in the haystack",
            "--prefilter",
            "all",
            "-v",
            "--api",
            &server.uri(),
            dir.path().to_str().unwrap(),
        ],
        &cache_env,
        None,
    );
    assert_eq!(r.code, 0);
    assert!(r.stdout.contains("nothing to see"));
    assert!(!r.stdout.contains("has the needle here"));
}

#[test]
fn json_output_shape() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let server = mock_server(&rt, "needle");
    let (_c, cache_env) = fresh_cache();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "has the needle here\n").unwrap();
    let r = run(
        &[
            "needle in the haystack",
            "--prefilter",
            "keywords",
            "--json",
            "--api",
            &server.uri(),
            dir.path().to_str().unwrap(),
        ],
        &cache_env,
        None,
    );
    assert_eq!(r.code, 0);
    let v: Value = serde_json::from_str(r.stdout.trim()).expect("valid JSON line");
    assert_eq!(v["kind"], "line");
    assert_eq!(v["start_line"], 1);
    assert!(v["p"].as_f64().unwrap() >= 0.5);
    assert_eq!(v["answers"]["d0"].as_f64().unwrap(), 0.9);
}

#[test]
fn cache_reuse_costs_zero_http_calls() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let server = mock_server(&rt, "needle");
    let (cache_dir, cache_env) = fresh_cache();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "has the needle here\n").unwrap();
    let args = [
        "needle in the haystack",
        "--prefilter",
        "keywords",
        "--api",
        &server.uri(),
        dir.path().to_str().unwrap(),
    ];
    let r1 = run(&args, &cache_env, None);
    assert_eq!(r1.code, 0);
    let r2 = run(&args, &cache_env, None);
    assert_eq!(r2.code, 0);
    assert_eq!(r1.stdout, r2.stdout);
    let received = rt
        .block_on(async { server.received_requests().await })
        .unwrap();
    assert_eq!(received.len(), 1, "second run must be served from cache");
    // The cache file exists.
    assert!(cache_dir.path().join("rg1/answers.v1.sqlite").exists());
}

#[test]
fn repeated_lines_share_one_state() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let server = mock_server(&rt, "needle");
    let (_c, cache_env) = fresh_cache();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("a.txt"),
        "has the needle here\nhas the needle here\nhas the needle here\n",
    )
    .unwrap();
    let r = run(
        &[
            "needle in the haystack",
            "--prefilter",
            "keywords",
            "--api",
            &server.uri(),
            dir.path().to_str().unwrap(),
        ],
        &cache_env,
        None,
    );
    assert_eq!(r.code, 0);
    assert_eq!(r.stdout.lines().count(), 3, "stdout: {}", r.stdout);
    let received = rt
        .block_on(async { server.received_requests().await })
        .unwrap();
    assert_eq!(received.len(), 1);
    let body: Value = serde_json::from_slice(&received[0].body).unwrap();
    let states = body["states"].as_array().unwrap();
    assert_eq!(states.len(), 1, "3 identical lines must dedup to 1 state");
}

#[test]
fn retries_on_503() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let server = rt.block_on(async {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/systemone/batches"))
            .respond_with(ResponseTemplate::new(503).set_body_string("server busy"))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/systemone/batches"))
            .respond_with(DecideLaya {
                keyword: "needle",
                p_match: 0.9,
                p_other: 0.1,
            })
            .mount(&server)
            .await;
        server
    });
    let (_c, cache_env) = fresh_cache();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "has the needle here\n").unwrap();
    let r = run(
        &[
            "needle in the haystack",
            "--prefilter",
            "keywords",
            "--api",
            &server.uri(),
            dir.path().to_str().unwrap(),
        ],
        &cache_env,
        None,
    );
    assert_eq!(r.code, 0, "stderr: {}", r.stderr);
    assert!(r.stdout.contains("has the needle here"));
    let received = rt
        .block_on(async { server.received_requests().await })
        .unwrap();
    assert_eq!(received.len(), 2, "503 retry must issue a second request");
}

#[test]
fn budget_halt_stops_issuing_requests() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let server = mock_server(&rt, "needle");
    let (_c, cache_env) = fresh_cache();
    let dir = tempfile::tempdir().unwrap();
    let mut lines = String::new();
    for i in 0..6 {
        lines.push_str(&format!("needle line {i}\n"));
    }
    std::fs::write(dir.path().join("a.txt"), lines).unwrap();
    let r = run(
        &[
            "needle line",
            "--prefilter",
            "keywords",
            "--batch-size",
            "1",
            "-j",
            "1",
            "--budget",
            "50",
            "--api",
            &server.uri(),
            dir.path().to_str().unwrap(),
        ],
        &cache_env,
        None,
    );
    let received = rt
        .block_on(async { server.received_requests().await })
        .unwrap();
    assert!(
        received.len() < 6,
        "budget must stop issuing requests; got {}",
        received.len()
    );
    assert!(r.stderr.contains("budget reached"), "stderr: {}", r.stderr);
}

#[test]
fn estimate_needs_no_server() {
    let (_c, cache_env) = fresh_cache();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "has the needle here\nnothing\n").unwrap();
    let r = run(
        &[
            "needle in the haystack",
            "--prefilter",
            "keywords",
            "--estimate",
            "--api",
            "http://127.0.0.1:1",
            dir.path().to_str().unwrap(),
        ],
        &cache_env,
        None,
    );
    assert_eq!(r.code, 0, "stderr: {}", r.stderr);
    assert!(r.stdout.contains("est. input tokens"));
    assert!(r.stdout.contains("candidates: 1"));
}

#[test]
fn emit_records_outputs_ids() {
    let (_c, cache_env) = fresh_cache();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "one\ntwo\n").unwrap();
    let r = run(
        &[
            "x",
            "--prefilter",
            "all",
            "--emit-records",
            "--api",
            "http://127.0.0.1:1",
            dir.path().to_str().unwrap(),
        ],
        &cache_env,
        None,
    );
    assert_eq!(r.code, 0, "stderr: {}", r.stderr);
    let lines: Vec<Value> = r
        .stdout
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["body"], "one");
    assert!(lines[0]["id"].as_str().unwrap().len() == 16);
}

#[test]
fn keywords_prefilter_limits_candidates() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let server = mock_server(&rt, "needle");
    let (_c, cache_env) = fresh_cache();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("a.txt"),
        "has the needle here\nunrelated alpha\nunrelated beta\nunrelated gamma\n",
    )
    .unwrap();
    let r = run(
        &[
            "needle in the haystack",
            "--prefilter",
            "keywords",
            "--api",
            &server.uri(),
            dir.path().to_str().unwrap(),
        ],
        &cache_env,
        None,
    );
    assert_eq!(r.code, 0);
    let received = rt
        .block_on(async { server.received_requests().await })
        .unwrap();
    assert_eq!(received.len(), 1);
    let body: Value = serde_json::from_slice(&received[0].body).unwrap();
    let states = body["states"].as_array().unwrap();
    assert_eq!(
        states.len(),
        1,
        "only the keyword-matching line is a candidate"
    );
    assert!(states[0]["state"].as_str().unwrap().contains("needle"));
}

#[test]
fn ordered_output_matches_input_order() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let server = mock_server(&rt, "needle");
    let (_c, cache_env) = fresh_cache();
    let dir = tempfile::tempdir().unwrap();
    let mut lines = String::new();
    for i in 0..8 {
        lines.push_str(&format!("needle row {i}\n"));
    }
    std::fs::write(dir.path().join("a.txt"), lines).unwrap();
    let r = run(
        &[
            "needle row",
            "--prefilter",
            "keywords",
            "--batch-size",
            "2",
            "-j",
            "4",
            "--api",
            &server.uri(),
            dir.path().to_str().unwrap(),
        ],
        &cache_env,
        None,
    );
    assert_eq!(r.code, 0, "stderr: {}", r.stderr);
    let out: Vec<&str> = r.stdout.lines().collect();
    assert_eq!(out.len(), 8);
    for (i, l) in out.iter().enumerate() {
        assert!(l.contains(&format!("needle row {i}")), "out of order: {l}");
    }
}

#[test]
fn stdin_input_mode() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let server = mock_server(&rt, "needle");
    let (_c, cache_env) = fresh_cache();
    let r = run(
        &[
            "needle in the haystack",
            "--prefilter",
            "keywords",
            "--api",
            &server.uri(),
        ],
        &cache_env,
        Some("has the needle here\nnothing\n"),
    );
    assert_eq!(r.code, 0, "code={} stderr: {}", r.code, r.stderr);
    assert!(
        r.stdout.contains("has the needle here"),
        "code={} stdout: {:?} stderr: {:?}",
        r.code,
        r.stdout,
        r.stderr
    );
    assert!(!r.stdout.contains("nothing"));
}

#[test]
fn falls_back_to_single_endpoint() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let server = rt.block_on(async {
        let server = MockServer::start().await;
        // Only the single endpoint is mounted; batches 404s.
        Mock::given(method("POST"))
            .and(path("/v1/systemone"))
            .respond_with(DecideLaya {
                keyword: "needle",
                p_match: 0.9,
                p_other: 0.1,
            })
            .mount(&server)
            .await;
        server
    });
    let (_c, cache_env) = fresh_cache();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "has the needle here\n").unwrap();
    let r = run(
        &[
            "needle in the haystack",
            "--prefilter",
            "keywords",
            "--api",
            &server.uri(),
            dir.path().to_str().unwrap(),
        ],
        &cache_env,
        None,
    );
    assert_eq!(r.code, 0, "stderr: {}", r.stderr);
    assert!(r.stdout.contains("has the needle here"));
}

#[test]
fn count_and_files_with_matches() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let server = mock_server(&rt, "needle");
    let (_c, cache_env) = fresh_cache();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "needle one\nneedle two\nother\n").unwrap();
    let r = run(
        &[
            "needle one",
            "--prefilter",
            "keywords",
            "-c",
            "--api",
            &server.uri(),
            dir.path().to_str().unwrap(),
        ],
        &cache_env,
        None,
    );
    assert_eq!(r.code, 0);
    assert!(r.stdout.contains(":2"), "stdout: {}", r.stdout);

    let r = run(
        &[
            "needle one",
            "--prefilter",
            "keywords",
            "-l",
            "--api",
            &server.uri(),
            dir.path().to_str().unwrap(),
        ],
        &cache_env,
        None,
    );
    assert_eq!(r.code, 0);
    assert_eq!(r.stdout.trim(), dir.path().join("a.txt").to_str().unwrap());
}

#[test]
fn color_always_emits_ansi_and_json_never_does() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let server = mock_server(&rt, "needle");
    let (_c, cache_env) = fresh_cache();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "has the needle here\n").unwrap();
    let args = [
        "needle in the haystack",
        "--prefilter",
        "keywords",
        "-n",
        "-o",
        "--api",
        &server.uri(),
        dir.path().to_str().unwrap(),
    ];
    // Piped stdout + auto -> no color.
    let r = run(&args, &cache_env, None);
    assert_eq!(r.code, 0);
    assert!(
        !r.stdout.contains("\x1b["),
        "auto must not color when piped"
    );
    // --color=always -> colored.
    let r = run(
        &[args.as_slice(), &["--color", "always"]].concat(),
        &cache_env,
        None,
    );
    assert_eq!(r.code, 0);
    assert!(
        r.stdout.contains("\x1b["),
        "always must color: {:?}",
        r.stdout
    );
    assert!(
        r.stdout.contains("\x1b[1;31mneedle\x1b[0m"),
        "body must highlight keywords: {:?}",
        r.stdout
    );
    // --color=never -> explicit off.
    let r = run(
        &[args.as_slice(), &["--color", "never"]].concat(),
        &cache_env,
        None,
    );
    assert!(!r.stdout.contains("\x1b["));
    // --json is machine-readable: never colored.
    let r = run(
        &[
            "needle in the haystack",
            "--prefilter",
            "keywords",
            "--json",
            "--color",
            "always",
            "--api",
            &server.uri(),
            dir.path().to_str().unwrap(),
        ],
        &cache_env,
        None,
    );
    assert!(!r.stdout.contains("\x1b["), "json must never be colored");
}

#[test]
fn probability_column_defaults_on() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let server = mock_server(&rt, "needle");
    let (_c, cache_env) = fresh_cache();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "has the needle here\n").unwrap();
    let base = [
        "needle in the haystack",
        "--prefilter",
        "keywords",
        "--api",
        &server.uri(),
        dir.path().to_str().unwrap(),
    ];
    // Default: probability column shown (proof the decision engine judged it).
    let r = run(&base, &cache_env, None);
    assert_eq!(r.code, 0, "stderr: {}", r.stderr);
    assert!(r.stdout.contains("0.900\t"), "stdout: {:?}", r.stdout);
    // --no-probability: plain grep-style output.
    let r = run(
        &[base.as_slice(), &["--no-probability"]].concat(),
        &cache_env,
        None,
    );
    assert_eq!(r.code, 0);
    assert!(!r.stdout.contains("0.900"), "stdout: {:?}", r.stdout);
    assert!(r.stdout.contains("has the needle here"));
}

#[test]
fn live_laya_studio() {
    if std::env::var("RUN_LIVE").ok().as_deref() != Some("1") {
        eprintln!("skipping live test (set RUN_LIVE=1)");
        return;
    }
    let (cache, cache_env) = fresh_cache();
    let _ = cache;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("greet.py"),
        "def main():\n    \"\"\"Print a greeting.\"\"\"\n    print(\"hello world\")\n\ndef helper():\n    return 42\n",
    )
    .unwrap();
    let r = run(
        &[
            "python code that prints a greeting",
            "--prefilter",
            "keywords",
            "-n",
            "-o",
            dir.path().to_str().unwrap(),
        ],
        &cache_env,
        None,
    );
    assert_eq!(r.code, 0, "stderr: {}", r.stderr);
    assert!(
        r.stdout.contains("Print a greeting"),
        "stdout: {}",
        r.stdout
    );
}
