# rg1

[![ci](https://github.com/eformat/rg1/actions/workflows/ci.yml/badge.svg)](https://github.com/eformat/rg1/actions/workflows/ci.yml)

**ripgrep, but the pattern is a description.**

`rg1` wraps [ripgrep](https://github.com/BurntSushi/ripgrep)'s engine (its
`grep-regex`/`grep-searcher`/`ignore` crates, linked in-process — no `rg`
install needed) to generate fast candidate records, then asks a
[decision engine](#the-decision-engine) one yes/no question per record:
*"does this text fit the description?"* Records that clear a probability
threshold are emitted grep-style.

```
rg1 "python code that prints a greeting" ~/src
```

```
src/hello.py:4:    """Print a warm greeting."""
```

It is the Rust sibling of [jgrep](https://github.com/keltokhy/jgrep) — grep-shaped
CLI, grep-shaped exit codes, but the pattern is a natural-language description
and the match is a model's judgment, not a regex.

## How it works

```
description ──▶ keyword regex ──▶ ripgrep engine ──▶ candidates ──▶ laya /v1/systemone ──▶ P(fits) ≥ 0.5 ──▶ output
   "a greeting"   (?i)greeting     (fast, local)      (batched)      (one call/record)   threshold       (grep-style)
```

1. **Candidates.** `--prefilter=keywords` (the default) derives a case-insensitive
   keyword alternation from the description and lets ripgrep's engine find
   candidate lines (fast and cheap; misses matches with zero lexical overlap).
   `--prefilter=all` judges every record.
2. **Judge.** Candidates are batched (≤1024 states/request) and sent to the
   decision engine as `noul` questions — one call per record regardless of how
   many `-e` descriptions you pass. Identical records dedup into one state;
   repeated runs hit an on-disk cache.
3. **Emit.** `answers.d0.noul` (P(true)) is compared against `-p/--threshold`
   (default 0.5); matches stream in input order with grep flags, and exit
   codes follow grep: `0` match, `1` no match, `2` error.

## Install

Prebuilt binaries (linux x86_64, macOS Apple Silicon) are attached to
each [GitHub Release](https://github.com/eformat/rg1/releases) — pushing a
`v*` tag builds and publishes them automatically with SHA256 checksums:

```sh
git tag v0.1.0 && git push origin v0.1.0
```

Or from source:

```sh
cargo install --path .   # or: cargo build --release / make install
```

## The decision engine

By default `rg1` talks to [**laya-studio**](https://github.com/eformat/laya-studio) ("decision 1" / systemone API):
an encoder-based typed-decision engine — one forward pass per record, no text
generation. Override with `--api` (any endpoint speaking the
`{model, state, questions}` protocol at `/v1/systemone[/batches]`):

```sh
rg1 "..." --prefilter all --api http://localhost:8080 .
```

No auth is used by default. `--model` selects the engine model (default `auto`).

## Usage

```
rg1 [OPTIONS] DESCRIPTION [PATH...]
```

The pattern is a description; PATHs are files or directories (default: stdin;
`-` means stdin; a directory with `--diff` runs `git log -p` in it).

### Input modes (mutually exclusive)

| Flag | Records |
|---|---|
| *(default)* | lines |
| `--para` | blank-line-separated paragraphs |
| `--whole` | whole files |
| `--chunks N --overlap M` | sliding windows of N lines |
| `--jsonl --field user.text` | JSON lines (dotted-path field) |
| `--csv --field message` | CSV rows (header or index field) |
| `--diff [-W]` | per-commit hunks from `git log -p` / format-patch streams |
| `--functions` | tree-sitter function bodies (python, go, c) |

### Selecting

| Flag | Meaning |
|---|---|
| `--prefilter keywords\|all` | candidate strategy (default `keywords`) |
| `-e DESC` | additional description (repeatable; all judged in one call) |
| `--all` | all descriptions must match (min instead of max of probabilities) |
| `-p, --threshold F` | match threshold (default 0.5) |
| `-v` | invert match |
| `-C N` | context lines around each judged line (marked `>`) |
| `-i` | (parity; the keyword prefilter is always case-insensitive) |

### Output

| Flag | Meaning |
|---|---|
| `-n` | line numbers |
| `-H` / `--no-filename` | force/suppress filenames |
| `-o` | probability column, **default on** — shows the decision engine's judgment (graded colors with `--color`) |
| `--no-probability` | disable the probability column |
| `-c` / `-l` | counts / files-with-matches |
| `-q` | exit status only |
| `--json` | one JSON object per match |
| `--record` | JSON with full record metadata (incl. diff hunk info) |
| `-m N` | stop after N matches per file |
| `--color auto\|always\|never` | color output (default `auto`: terminal + no `NO_COLOR`); magenta paths, green line numbers, graded probability (≥0.9 bold green / ≥0.7 green / else yellow), red-bold description keywords in bodies |

### Economics & runtime

| Flag | Meaning |
|---|---|
| `--api URL` | decision engine base URL |
| `--model M` | engine model (default `auto`) |
| `-j N` | concurrent requests (default 32) |
| `--batch-size N` | states per request (default 64, max 1024) |
| `--timeout S` | per-request timeout (default 15) |
| `--budget T` | input-token seat belt (default 1M; 0 = unlimited) |
| `--no-cache` | disable the answer cache |
| `--stats` | print run statistics to stderr |
| `--estimate` | offline plan preview (no API calls) |
| `--emit-records` | offline JSONL dump of the record stream |
| `--max-chars N` | clamp judged text (code units fail instead, never truncate) |
| `--glob/--exclude/--hidden/--no-ignore` | walk filters (ripgrep semantics) |

### Examples

```sh
# lines fitting a description, with probabilities
rg1 "code that handles authentication" --prefilter keywords -o -n src/

# every record (no prefilter) — like jgrep
rg1 "an angry customer complaint" --prefilter all --para tickets/

# multiple descriptions, ALL must fit, one API call per record either way
rg1 "error handling" -e "retry logic" --prefilter all --all -o src/

# judge commit hunks with enclosing-function context from the post-image blob
rg1 "a commit that changes pricing math" --diff -W --prefilter all -o -H .

# pipe git log -p yourself — narrow the window first (cheapest)
git log -p --since="last week" | rg1 "a bug fix" --diff --prefilter all -o -H -n
git log -p -- author@example.com -- src/auth/ | rg1 "credential handling" --diff -W --max-chars 50000 --prefilter all -o

# uncommitted changes (plain git diff works too)
git diff | rg1 "refactoring of the router" --diff --prefilter all -o

# patch streams (format-patch)
git format-patch -10 --stdout | rg1 "a security fix" --diff -W --prefilter all -o

# JSONL fields, CSV rows
rg1 "a bug report" --prefilter all --jsonl --field issue.text logs.jsonl
rg1 "a billing complaint" --prefilter all --csv --field message tickets.csv

# plan the run without spending a single call
rg1 "refactoring comments" --prefilter keywords --estimate .

# stream JSON matches to jq
rg1 "TODO comments" --prefilter keywords --json . | jq -r '.body'
```

## Semantics worth knowing

- **Exit codes** follow grep: `0` match, `1` no match, `2` error (including
  file errors and API failures after retries).
- **Blank records** short-circuit to p=0.0 with no API call.
- **Cache**: `~/.cache/rg1/answers.v1.sqlite` (honors `XDG_CACHE_HOME`),
  keyed on endpoint+model+state+description. Reruns and repeated lines cost
  zero HTTP calls.
- **Budget**: each request reserves its estimated token cost before sending,
  so concurrent in-flight requests cannot overshoot `--budget`. Cached answers
  always print; over-budget halts with a note on stderr.
- **Diff hunks and functions are never silently truncated** — oversized units
  fail with size + location (`--max-chars`, default 8000).
- **`-W` fallbacks**: when enclosing-function context can't be attached
  (`deleted_file`, `outside_function`, `function_in_hunk`, `too_large`, …)
  the hunk is still judged alone; `--stats` shows the counts.
- **Retries**: 503/5xx and network errors retry with backoff (honoring
  `Retry-After`); 4xx is fatal. Deployments without the batch endpoint fall
  back to single-record requests automatically.
- **Broken pipe** (`rg1 ... | head -1`) exits 141 quietly.

## Architecture

```
src/
  main.rs      entry, tokio, grep exit codes
  cli.rs       clap parser + validation + question instructions
  prefilter.rs description -> keyword alternation regex (or judge-all)
  search.rs    grep-regex + grep-searcher candidate lines (ripgrep's engine)
  inputs.rs    discovery (ignore crate) + line/para/whole/chunk records
  data.rs      jsonl/csv records with dotted-path fields
  diff.rs      git log -p / format-patch -> per-commit hunks (unidiff)
  code.rs      tree-sitter function spans (python, go, c)
  gitctx.rs    hardened git blob access for -W (fixed-arg, capped, contained)
  laya.rs      decision-engine client: batches, retries, single fallback
  judge.rs     states -> cache -> concurrent batches -> ordered decisions
  cache.rs     SQLite answer cache
  emit.rs      grep-compatible output
  color.rs     ANSI painter (--color auto|always|never, NO_COLOR-aware)
  estimate.rs  --estimate / --emit-records
  stats.rs     run statistics + token-budget accounting
```

## Tests

```sh
cargo test                 # unit + integration (mock laya server via wiremock)
RUN_LIVE=1 cargo test      # also run against the real laya-studio endpoint
```

## License

MIT
