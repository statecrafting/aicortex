# aicortex: the one source of truth for what CI validates (spec 001 B-2).
#
# Every target is guarded so the composite is green on the specify-only tree:
# before spec 010 lands there is no Cargo.toml.
# `make ci` locally means a green CI run.
#
# The gate is split the way spec-spine's kit splits it (spec-spine spec 064):
# `make gate` reads and never writes, `make refresh` is the writing half, and
# `make spine` is the two in order for a live session that can commit the
# regenerated shards. A gate that writes repairs what it exists to judge.

SHELL := /bin/bash
.DEFAULT_GOAL := ci

SPEC_SPINE ?= spec-spine
# The coupling base follows the branch this repository actually has
# (spec-spine spec 072): $SPEC_SPINE_DEFAULT_BRANCH (make imports the
# environment, so `?=` leaves an exported value alone), then the remote's own
# HEAD, then `main`. An explicit `BASE=` on the command line still wins.
SPEC_SPINE_DEFAULT_BRANCH ?= $(shell git symbolic-ref --short refs/remotes/origin/HEAD 2>/dev/null | sed 's|^origin/||')
BASE ?= origin/$(or $(SPEC_SPINE_DEFAULT_BRANCH),main)

.PHONY: gate refresh spine spec-dag ci build test lint fmt deny coverage attest verify help

## gate: the governed chain, read-only (check, lint, couple, spec-dag)
gate:
	$(SPEC_SPINE) check --fail-on-warn
	$(SPEC_SPINE) lint --fail-on-warn
	$(SPEC_SPINE) couple --base $(BASE) --head HEAD
	scripts/spec-dag.sh

## refresh: recompute the committed shard trees (the writing half)
refresh:
	$(SPEC_SPINE) compile
	$(SPEC_SPINE) index

## spine: refresh then gate, for a session that can commit the regenerated shards
spine:
	$(MAKE) refresh
	$(MAKE) gate

## spec-dag: depends_on is acyclic and only names lower-numbered specs
spec-dag:
	scripts/spec-dag.sh

## ci: everything CI runs, in order
ci: spine
	$(SPEC_SPINE) index coverage
	@if [ -f Cargo.toml ]; then $(SPEC_SPINE) index coverage --fail-on-untraced; else echo "coverage: no Cargo.toml yet, reported above and not refused (spec 001 D-7)"; fi
	$(MAKE) build
	$(MAKE) test
	$(MAKE) lint
	$(MAKE) fmt
	$(MAKE) deny

## build: cargo build (guarded on Cargo.toml)
build:
	@if [ -f Cargo.toml ]; then cargo build --workspace --locked; else echo "build: no Cargo.toml yet (lands with spec 010)"; fi

## test: cargo test (guarded on Cargo.toml)
test:
	@if [ -f Cargo.toml ]; then cargo test --workspace --locked; else echo "test: no Cargo.toml yet (lands with spec 010)"; fi

## lint: clippy with warnings denied (guarded on Cargo.toml)
lint:
	@if [ -f Cargo.toml ]; then cargo clippy --workspace --all-targets --locked -- -D warnings; else echo "lint: no Cargo.toml yet (lands with spec 010)"; fi

## fmt: rustfmt check (guarded on Cargo.toml)
fmt:
	@if [ -f Cargo.toml ]; then cargo fmt --all --check; else echo "fmt: no Cargo.toml yet (lands with spec 010)"; fi

## deny: cargo-deny supply-chain check (guarded on deny.toml and the tool)
deny:
	@if [ -f deny.toml ]; then \
	  if command -v cargo-deny >/dev/null 2>&1; then cargo deny check; \
	  else echo "deny: cargo-deny not installed (cargo install cargo-deny --locked); skipped locally, CI runs it"; fi; \
	else echo "deny: no deny.toml yet (lands with spec 010)"; fi

## coverage: which source files no spec specifically claims
coverage:
	$(SPEC_SPINE) index coverage

## attest: the corpus attestation (spec-spine's ledger seal), never committed
attest:
	@mkdir -p .derived/attestation
	$(SPEC_SPINE) attest --with-coupling > .derived/attestation/corpus.json
	@echo "attestation written to .derived/attestation/corpus.json"

## verify: run one spec's declared acceptance, e.g. make verify SPEC=018-retrieval-and-recall-trace
verify:
	@test -n "$(SPEC)" || { echo "usage: make verify SPEC=<spec-id>"; exit 3; }
	$(SPEC_SPINE) verify $(SPEC)

## help: list targets
help:
	@grep -E '^## ' $(MAKEFILE_LIST) | sed 's/^## //'
