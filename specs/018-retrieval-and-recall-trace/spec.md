---
id: "018-retrieval-and-recall-trace"
title: "Recall: four channels fused into one order, every result explainable, scope in the SQL"
status: approved
kind: "kernel"
domain: "retrieval"
created: "2026-09-03"
authors: ["Bartek Kus"]
implementation: pending
risk: critical
wave: 1
depends_on:
  - "017-entities-and-typed-edges"
establishes:
  - "crates/aicortex-recall/Cargo.toml"
  - "crates/aicortex-recall/src/lib.rs"
  - "crates/aicortex-recall/src/query.rs"
  - "crates/aicortex-recall/src/channels.rs"
  - "crates/aicortex-recall/src/fusion.rs"
  - "crates/aicortex-recall/src/trace.rs"
  - "crates/aicortex-recall/src/filter.rs"
  - "crates/aicortex-recall/tests/fusion.rs"
  - "crates/aicortex-recall/tests/trace.rs"
  - "crates/aicortex-recall/testdata/queries/"
summary: >
  One query, four candidate channels (vector, text, recency, graph), one
  fused order, and a trace that explains it. Fusion is reciprocal rank with
  declared weights and an importance prior, chosen because it needs no score
  calibration between channels that do not share a scale. Every result set
  carries the query, the per-channel candidates and scores, the fusion
  arithmetic, and the reason each returned memory made the cut. A ranking
  that cannot be explained is not shippable, and an evaluation corpus (040)
  is the only way a weight changes.
---

# 018: Recall

## 1. Purpose

Retrieval is where a memory system is judged. The predecessor offered
cosine similarity with an optional JSON containment filter, which cannot
answer a query about a person, cannot prefer what is recent, and cannot say
why it returned what it returned. Its users responded by writing recency
boosts, trigram search, source filtering, and schema-aware routing
themselves.

This spec puts those in the core and adds the thing none of them had: a
trace. An unexplainable ranking cannot be debugged, cannot be evaluated,
and cannot be trusted by the person deciding whether to act on a memory.

## 2. Territory

The `aicortex-recall` crate. It reads through the repositories and the
index and writes nothing except a trace record when asked. It owns no SQL
against the memory tables.

## 3. Behavior

- **B-1 (the query).** `Query { scope, text, filter: Filter, k: usize,
  channels: ChannelSet, budget: Duration, explain: bool }`. `Filter`
  covers memory kind, trust class, status, actor kind, source, entity,
  and a time range. Filters are predicates in the SQL of each channel, not
  a post-pass (constitution VII and 012 B-3).
- **B-2 (channels).** Four, each returning at most `k * overfetch`
  candidates with a channel score: `Vector` (016 B-2, over the active
  revision), `Text` (016 B-5, BM25), `Recency` (a decay over `created`,
  bounded so it can surface but never dominate), and `Graph` (017 B-9,
  memories reached from entities mentioned in the query, scored by inverse
  path length). Channels run concurrently and each is subject to the
  shared budget.
- **B-3 (fusion).** Reciprocal rank fusion: a memory's fused score is the
  weighted sum over channels of `1 / (rank_constant + rank)`, multiplied by
  the decayed importance prior (011 B-9), with weights from configuration
  and a default set recorded in this spec. RRF is chosen because vector
  cosine, BM25, recency decay, and path length have no common scale and
  calibrating them would be a per-corpus tuning exercise that never
  converges.
- **B-4 (chunk to memory).** A memory's score is its best chunk's score
  (015 D-2), and the winning chunk's byte range travels with the result so
  a citation points at a sentence.
- **B-5 (the trace).** `RecallTrace { query_hash, filters, per_channel:
  Vec<ChannelTrace>, fused: Vec<FusedEntry>, weights, budget_used,
  truncated }` where `ChannelTrace` holds the channel's candidates, their
  raw scores, and their ranks. With `explain: false` the trace is still
  computed but summarized to per-channel counts and the winning reasons,
  because the cost of the full trace is memory, not work.
- **B-6 (persisted traces).** A trace may be persisted against a scope for
  later evaluation and debugging, keyed by `query_hash`, with a retention
  window and no memory bodies inside it: it holds ids, scores, and ranks.
  Persistence is off by default and on for the evaluation corpus.
- **B-7 (status and quarantine).** Only `Active` memories are candidates.
  `Quarantined`, `Superseded`, `Expired`, and `Erased` are excluded in
  every channel's SQL. A caller may explicitly request superseded or
  expired rows, which is recorded in the trace, and may never request
  quarantined or erased ones.
- **B-8 (determinism).** For a fixed corpus, query, filter, weights, and
  clock, the returned order is identical across runs. The clock is
  injected, so recency is testable.
- **B-9 (budget).** The query budget is shared across channels. A channel
  that exceeds it contributes what it has, marked truncated, and the result
  set is marked truncated. Truncation is always visible in the trace and
  counted in metrics.
- **B-10 (weights change only with evidence).** The default weights are
  recorded here. A change to them, to the rank constant, or to the recency
  half-life requires an evaluation run (040) reported in the pull request.

## 4. Functional requirements

- **FR-001.** Over the committed query fixtures, the returned order matches
  the recorded expectation exactly, with the injected clock.
- **FR-002.** Disabling a channel changes the order in the documented
  direction and never panics; enabling only one channel returns that
  channel's order.
- **FR-003.** A quarantined and an erased memory are absent from every
  channel's candidates, asserted per channel rather than only on the fused
  result.
- **FR-004.** A recall over a scope containing another scope's identical
  content returns nothing from the other scope.
- **FR-005.** The trace's fused arithmetic reproduces the returned order
  when recomputed from the per-channel ranks in the trace.
- **FR-006.** A budget of one millisecond over a large fixture returns a
  non-empty truncated result rather than an error.
- **FR-007.** A persisted trace contains no memory body, asserted by
  searching the serialized trace for a distinctive fixture token.

## 5. Acceptance criteria

- **AC-1.** `cargo test -p aicortex-recall --locked` passes.
- **AC-2.** The evaluation corpus (040) runs against this implementation
  and records a baseline for recall at 5, 10, and 20.

## 6. Out of scope

How results are framed for a model, which is 019 and is a hard boundary on
this spec's output. Tool and API shapes (020, 021). Reranking with a second
model, which is deferred until evaluation justifies it. Query rewriting and
expansion, likewise.

## 7. Resolved decisions

- **D-1 (2026-09-03, this spec).** Reciprocal rank fusion rather than
  normalized score blending. Blending requires per-corpus calibration of
  four incomparable scales and drifts as the corpus grows; RRF depends only
  on rank and is stable.
- **D-2 (2026-09-03, this spec).** The trace is always computed, and
  `explain` controls only its verbosity. A trace computed only on demand
  is a different code path from the one that produced the result, which is
  precisely when explanations start to lie.

## Verification

```verify:cli
cargo test -p aicortex-recall --locked
```
