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
# make clean      remove build artifacts

BIN       := target/release/rg1

# Run configuration: make run DESC='"a bug report"' DIR=. PREFILTER=keywords EXTRA="-o -n -C 2"
DESC      ?= "python code that prints a greeting"
DIR       ?= .
PREFILTER ?= keywords
EXTRA     ?=

.PHONY: build release test live check clippy fmt fmt-check install run demo clean

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

clean:
	cargo clean
