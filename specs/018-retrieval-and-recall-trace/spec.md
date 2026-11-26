---
id: "018-retrieval-and-recall-trace"
title: "Recall: four channels fused into one order, every result explainable, scope in the SQL"
status: approved
kind: "kernel"
domain: "retrieval"
created: "2026-09-03"
authors: ["Bartek Kus"]
implementation: in-progress
risk: critical
wave: 1
depends_on:
  - "017-entities-and-typed-edges"
establishes:
  - "crates/aicortex-recall/Cargo.toml"
  - "crates/aicortex-recall/src/lib.rs"
  - "crates/aicortex-recall/src/query.rs"
  - "crates/aicortex-recall/src/channels.rs"
  - "crates/aicortex-recall/src/freshness.rs"
  - "crates/aicortex-recall/src/recall.rs"
  - "crates/aicortex-recall/tests/common/mod.rs"
  - "crates/aicortex-recall/src/fusion.rs"
  - "crates/aicortex-recall/src/trace.rs"
  - "crates/aicortex-recall/src/filter.rs"
  - "crates/aicortex-recall/tests/fusion.rs"
  - "crates/aicortex-recall/tests/trace.rs"
  - "crates/aicortex-recall/testdata/queries/"
extends:
  - { spec: "016-vector-index-and-scan", unit: "crates/aicortex-index/src/lib.rs", nature: additive }
  - { spec: "016-vector-index-and-scan", unit: "crates/aicortex-index/src/scan.rs", nature: additive }
  - { spec: "016-vector-index-and-scan", unit: "crates/aicortex-index/src/text.rs", nature: additive }
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
- **B-11 (freshness signals; added 2026-10-25, D-3).** Every result set
  carries a `Freshness` block in its response metadata, so a client can
  tell the user that the store holds nothing recent about a subject
  without the server writing that sentence (`gbrain://gap-note`). It holds
  two lists. Per entity resolved from the query by the graph channel (017
  B-9): the entity id, the capture time of the newest `Active` memory that
  mentions it, and the count of such memories. Per source among the
  returned results and any source named in the filter: the source kind and
  reference as 011 records them, the capture time of the newest `Active`
  memory from that source, and the count. Both lists are computed in the
  caller's scope with the same status predicates as B-7, so an erased or
  quarantined memory never moves a newest time, and they ignore the
  query's time-range filter, because a gap is only visible against the
  whole scope. The block holds ids, kinds, times, and counts only: no memory
  body, entity name, alias, or generated sentence, which keeps it outside
  019's envelope (019 B-6) and inside the trace rule of B-6. Each list is
  bounded (default 16 entries) in a fixed order, newest time descending
  then id, and is marked truncated when cut. The newest times are read at
  the injected clock, so B-8 holds. The block is copied into the trace.

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
- **FR-008.** For a fixture entity that the graph channel resolves from
  the query text (B-11), whose newest mentioning memory ranks outside the
  top `k`, the `Freshness` block still reports that memory's capture time
  and the full count; erasing that memory moves the newest time back to
  the next one. A fixture entity that appears only in results and is not
  resolved from the query is absent from the per-entity list.
- **FR-009.** The serialized `Freshness` block of a fixture query contains
  no memory body, entity name, or alias, asserted by searching it for
  distinctive fixture tokens, and the block is byte-identical across runs
  at a fixed clock.

## 5. Acceptance criteria

- **AC-1.** `cargo test -p aicortex-recall --locked` passes, and it
  contains a test for each of FR-001 to FR-009, the freshness
  requirements FR-008 and FR-009 included.
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

- **D-3 (2026-10-25, amendment, pending owner review).** Added B-11 and
  FR-008 to FR-009: freshness signals in the response metadata. The
  observation is `gbrain://gap-note` (`docs/design/00-lineage.md`): the
  answer feature users valued most was a note that the store holds nothing
  about a subject since a given date. The server returns the newest
  capture time per resolved entity and per source, with counts, and leaves
  the sentence to the client, because a server-written sentence would be
  derived content needing an envelope (019 B-5) and a language decision
  this crate does not own. The amendment adds behavior to an approved spec
  that no human has reviewed, so this spec returns to `status: draft`
  (constitution V) until the owner approves it. Not carried: an adapter's
  last successful sync time, the strongest staleness signal, which belongs
  to the source adapter framework (030); 030 may add it to this block
  through an `extends` edge. The evidence unit and token budget observed
  alongside this one are spec 025's, not this spec's: B-4 governs ranking,
  and what text is delivered under a token budget is a packing stage over
  019's framing that section 6 already places outside this spec.

- **D-4 (2026-10-30, owner approval).** The owner reviewed the D-3
  amendment and approved this spec; it returns to `status: approved`.

- **D-5 (2026-11-26, build).** Dependency edges of `aicortex-recall`,
  landed in their own commit apart from the code: `aicortex-index` (vector
  and text channels, 016), `aicortex-embed` (the query embedder and the
  active model revision, 015), `aicortex-graph` (entity walk, 017),
  `aicortex-store` (scope ids, the read seam), `rahi-store` (the store
  handle), `ring` (the SHA-256 `query_hash`), and `tokio` (`join!` runs the
  channels concurrently; no runtime is started here). All are existing
  workspace entries; `tempfile` joins as a dev-dependency. Every edge points
  downward.

- **D-6 (2026-11-26, build).** B-1 requires every filter predicate to be in
  each channel's SQL. Spec 016's scan and search carried only a status list,
  so 018 extends them additively (`extends` edges above): a typed
  `Restriction` (kinds, trusts, actor kinds, source systems, entities, a
  created range) that `Filter::restricted` carries and the scan statement
  and the text statement render as fixed clauses over the `memory` alias,
  every value bound as a parameter, an empty field adding no clause. The
  recency and graph channels render the same `Restriction` in their own
  statements. Nothing is filtered after the fact. Recall owns SQL against
  `memory`, `provenance`, `mention`, and `entity_alias` for the recency
  channel, the graph admission check, importance priors, and B-11, because
  no repository exposes those reads; this refines the Territory sentence
  that recall owns no SQL against the memory tables, it does not widen
  what recall writes (nothing).
- **D-7 (2026-11-26, build).** Choices B-2, B-3, and B-11 left open, and
  the defaults B-3 asks this spec to record. Weights: vector 1.0, text 0.8,
  graph 0.6, recency 0.2. RRF constant 60. Overfetch 4 (candidates per
  channel are `k * 4`). Recency half-life 7 days, scored `0.5^(age /
  half_life)`, admitting only memories younger than 4 half-lives and not
  later than the injected clock, so it can surface but cannot dominate.
  Importance prior: `1 - span/2 + span * decayed` with span 0.5, so it
  ranges over 0.75 to 1.25 and never zeroes a memory; a record with no
  readable importance takes decayed 0.5 (prior 1.0). Because the prior
  multiplies the fused score, enabling one channel returns that channel's
  order (FR-002) when the candidates' priors are equal, and the prior may
  reorder adjacent ranks otherwise; FR-002's test holds importance equal.
  Graph channel: entities are those whose normalized alias occurs in the
  normalized query as whole words (016's `normalize`, at most 16); a memory
  that mentions one scores 1.0 (path length zero), a memory reached by an
  edge walk of `n` hops scores `1 / (1 + n)`; the walk and mention reads are
  followed by one admission statement that applies status and filter. B-11
  reads `provenance.captured_at` as the capture time, bounds it by the
  clock, and keys a source by system plus external id; entities are
  resolved for freshness whether or not the graph channel is enabled. A
  channel that cannot run (no active revision, a provider that is not the
  active model, an embedding failure) is skipped and says why in the trace,
  rather than failing the query. Default budget in `Query::new` is 2 seconds.
- **D-8 (2026-11-26, build).** Part 1 of 018 lands B-1 to B-5, B-7 to B-9,
  and B-11 with FR-001 to FR-009 over the real store, and leaves
  `implementation: in-progress`. Not in part 1: B-6's stored trace (a
  migration, a repository, the retention window, and the evaluation
  corpus switch: the trace serializes to body-free JSON today and FR-007 is
  asserted on that JSON), the metrics counter for truncation (B-9), a search
  route in `aicortex-api/src/search.rs` (020 D-17), and AC-2, which needs
  spec 040's corpus. No ranking change has run an evaluation: the corpus
  does not exist yet (B-10), so the defaults of D-7 are provisional until
  040 reports a baseline. The part's files are claimed in `establishes`
  above: `query.rs` and `channels.rs` (written there when this spec was
  approved), `fusion.rs`, `trace.rs`, `filter.rs`, `freshness.rs`,
  `recall.rs`, and the tests and `testdata/queries/`.
- **D-9 (2026-11-26, build).** A filter that admits superseded memories
  names no successor: recall builds the index filter from the store's
  payload-free `StatusFilter` through `aicortex_index::Filter::status_filters`,
  an additive constructor beside `Filter::statuses` (the D-6 edge on 016's
  `scan.rs`), so no placeholder `MemoryId` is minted.

## Verification

```verify:cli
cargo test -p aicortex-recall --locked
```
