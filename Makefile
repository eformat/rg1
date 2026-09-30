# rg1 — grep, but the pattern is a description
#
# make build      debug build                                    (default target)
# make release    optimized build
# make test       unit + integration tests (mock laya server)
# make live       also run the live test against laya-studio
# make check      fast type check
# make clippy     lint
# make fmt        format (fmt-check to verify only)
# make install    install the rg1 binary into ~/.cargo/bin
# make run        run against DIR with DESC:  make run DESC='"auth code"' DIR=src EXTRA="-o -n"
# make demo       one-line stdin demo against the live engine
# make release-tag  create + push a v* tag -> the release workflow builds and
#                   publishes the GitHub Release with binaries + SHA256SUMS
#                   (VERSION defaults to Cargo.toml; e.g. make release-tag VERSION=0.1.1)
# make clean      remove build artifacts

BIN       := target/release/rg1

# Run configuration: make run DESC='"a bug report"' DIR=. PREFILTER=keywords EXTRA="-o -n -C 2"
DESC      ?= "python code that prints a greeting"
DIR       ?= .
PREFILTER ?= keywords
EXTRA     ?=

# Release configuration: make release-tag [VERSION=0.1.1] [REMOTE=origin] [REPO=eformat/rg1]
VERSION   ?= $(shell sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
V         := $(VERSION:v%=%)
REMOTE    ?= origin
REPO      ?= eformat/rg1

.PHONY: build release test live check clippy fmt fmt-check install run demo release-tag clean

build:
	cargo build

release:
	cargo build --release

test:
	cargo test

live:
	RUN_LIVE=1 cargo test

check:
	cargo check

clippy:
	cargo clippy

fmt:
	cargo fmt

fmt-check:
	cargo fmt --check

install: release
	cargo install --path .

run: release
	$(BIN) --prefilter $(PREFILTER) $(DESC) $(DIR) $(EXTRA)

demo: release
	echo 'def main(): print("hello world")' | $(BIN) "python code that prints a greeting" --prefilter all -o

release-tag: fmt-check test
	@test -z "$$(git status --porcelain)" || { echo "rg1: working tree dirty; commit first"; exit 1; }
	@if git tag -l | grep -qx "v$(V)"; then echo "rg1: tag v$(V) already exists"; exit 1; fi
	git tag v$(V)
	git push $(REMOTE) v$(V)
	@echo "tag v$(V) pushed -> release workflow building binaries"
	@echo "watch: gh run watch --repo $(REPO)"
	@echo "release: https://github.com/$(REPO)/releases"

clean:
	cargo clean
