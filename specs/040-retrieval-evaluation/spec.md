---
id: "040-retrieval-evaluation"
title: "Evaluation: a golden corpus, recall and precision at k, and a gate that refuses a silent regression"
status: approved
kind: "kernel"
domain: "ops"
created: "2026-09-03"
authors: ["Bartek Kus"]
implementation: pending
risk: critical
wave: 4
depends_on:
  - "035-agent-coordination"
establishes:
  - "crates/aicortex-eval/Cargo.toml"
  - "crates/aicortex-eval/src/lib.rs"
  - "crates/aicortex-eval/src/corpus.rs"
  - "crates/aicortex-eval/src/metrics.rs"
  - "crates/aicortex-eval/src/runner.rs"
  - "crates/aicortex-eval/src/report.rs"
  - "crates/aicortex-eval/src/main.rs"
  - "crates/aicortex-eval/tests/metrics.rs"
  - "eval/README.md"
  - "eval/golden/"
  - "eval/baselines/"
extends:
  - { spec: "018-retrieval-and-recall-trace", unit: "crates/aicortex-recall/src/lib.rs", nature: additive }
summary: >
  Retrieval quality is the product, and it degrades invisibly. This spec
  builds the instrument: a committed golden corpus of memories and queries
  with judged relevance, a runner that loads it into a temporary instance
  and executes every query, metrics that are the standard ones rather than
  invented, a baseline file per configuration, and a gate that fails a
  change which moves a metric down by more than its tolerance without an
  explicit, justified baseline update. Constitution XIV becomes a command
  with an exit code.
---

# 040: Evaluation

## 1. Purpose

Every parameter in this system is a guess until it is measured: chunk size
and overlap, the fusion weights and rank constant, the recency half-life,
the consolidation thresholds, the entity resolution threshold, and the
choice of embedding model. Without an instrument, each change is argued
from intuition and the system drifts.

The predecessor's community wrote smoke tests, health monitors, and world
model diagnostics for exactly this reason, after the fact and per fork
(`openbrain://valued-capabilities`).

## 2. Territory

The `aicortex-eval` crate and the `eval/` tree. The corpus is committed
data, not generated at run time.

## 3. Behavior

- **B-1 (the corpus).** `eval/golden/` holds memory sets and query sets in
  a documented format: each query has judged relevant memory ids with a
  graded relevance, and a note on why. It covers, at minimum: exact
  lookup, paraphrase, multi-hop through an entity, temporal preference
  where the newest of several similar memories is correct, negation,
  a query whose answer was superseded and must return the correction, and
  a query with no relevant memory at all where returning nothing is
  correct.
- **B-2 (synthetic scale).** A generator produces a large corpus with known
  structure for latency measurement, seeded and reproducible, kept
  separate from the judged corpus so scale numbers never contaminate
  quality numbers.
- **B-3 (metrics).** Recall at 5, 10, and 20; precision at 5; mean
  reciprocal rank; normalized discounted cumulative gain at 10; the
  no-answer rate on queries whose correct result is empty; and the
  truncation rate. Latency at p50, p95, and p99 per channel and fused.
  Definitions are the standard ones and are stated in `metrics.rs` with
  their formulas.
- **B-4 (the runner).** `aicortex-eval run --corpus <name> --config
  <profile>` boots a temporary instance through rahi's harness, imports
  the corpus, waits for embedding to drain, executes every query, and
  writes a report. It is hermetic: no network, the local provider, a fixed
  seed, and an injected clock.
- **B-5 (baselines).** `eval/baselines/<profile>.toml` records the last
  accepted metrics with the commit and date. `aicortex-eval check`
  compares a fresh run to the baseline and exits non-zero when any metric
  falls by more than its declared tolerance.
- **B-6 (the gate).** `make eval` runs `check` for the default profile.
  CI runs it when any file owned by 015, 016, 017, 018, or 034 changes,
  detected through the spec-spine index rather than a path glob. A change
  that moves a metric down must either be fixed or accompanied by a
  baseline update whose commit message states the tradeoff, which is a
  human decision visible in review.
- **B-7 (reports are diffable).** The report is deterministic text and
  includes per-query deltas against the baseline, so a reviewer sees which
  queries got worse rather than only that the mean moved.
- **B-8 (the corpus is honest).** Judgments record who made them and when.
  A query added because it failed is marked as such, so the corpus does
  not silently become a set of things that already pass.

## 4. Functional requirements

- **FR-001.** `aicortex-eval run` on the committed corpus is deterministic:
  two runs produce identical reports.
- **FR-002.** `check` fails when fusion weights are perturbed enough to
  move recall at 10 below tolerance, and passes when they are unchanged.
- **FR-003.** Metric implementations match hand-computed values on a small
  fixture, asserted in `tests/metrics.rs`.
- **FR-004.** The runner completes the judged corpus within its time
  budget on CI hardware and reports latency percentiles.
- **FR-005.** The no-answer query returns an empty result, and returning
  anything scores zero for it.

## 5. Acceptance criteria

- **AC-1.** `cargo test -p aicortex-eval --locked` passes.
- **AC-2.** `make eval` exits 0 against the committed baseline, and the
  baseline for the default profile is populated from a real run.

## 6. Out of scope

End-to-end quality of an assistant using this store, which depends on the
client. Benchmarking against other memory products, which is marketing
rather than a gate.

## 7. Resolved decisions

- **D-1 (2026-09-03, this spec).** The corpus is committed rather than
  generated. A generated corpus measures the generator's assumptions;
  judged data is the only thing that can contradict a design belief.
- **D-2 (2026-09-03, this spec).** The gate is triggered by ownership in
  the spec index rather than by a path glob, so a new file in a
  retrieval-owning spec is covered on the day it lands.

## Verification

```verify:cli
cargo test -p aicortex-eval --locked
```
