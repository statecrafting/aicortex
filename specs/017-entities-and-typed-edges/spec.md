---
id: "017-entities-and-typed-edges"
title: "Entities and typed edges: a closed predicate vocabulary, derived facts that never outrank their source"
status: approved
kind: "kernel"
domain: "memory"
created: "2026-09-03"
authors: ["Bartek Kus"]
implementation: complete
risk: high
wave: 1
depends_on:
  - "016-vector-index-and-scan"
establishes:
  - "crates/aicortex-graph/Cargo.toml"
  - "crates/aicortex-graph/src/lib.rs"
  - "crates/aicortex-graph/src/entity.rs"
  - "crates/aicortex-graph/src/resolve.rs"
  - "crates/aicortex-graph/src/edge.rs"
  - "crates/aicortex-graph/src/vocabulary.rs"
  - "crates/aicortex-graph/src/extract.rs"
  - "crates/aicortex-graph/src/walk.rs"
  - "crates/aicortex-graph/src/migrations.rs"
  - "crates/aicortex-graph/src/worker.rs"
  - "crates/aicortex-graph/src/preflight.rs"
  - "crates/aicortex-graph/tests/common/mod.rs"
  - "crates/aicortex-graph/tests/extract.rs"
  - "crates/aicortex-graph/tests/worker.rs"
  - "crates/aicortex-store/src/graph_tables.rs"
  - "crates/aicortex-store/tests/graph_erasure.rs"
  - "crates/aicortex-graph/tests/resolve.rs"
  - "crates/aicortex-graph/tests/edge.rs"
  - "crates/aicortex-graph/testdata/extraction/"
  - "apps/aicortex/src/graph_preflight.rs"
  - "apps/aicortex/src/graph_service.rs"
  - "apps/aicortex/tests/graph.rs"
  - "crates/aicortex-api/tests/entities.rs"
extends:
  - { spec: "012-store-schema-and-repositories", unit: "crates/aicortex-store/src/migrations.rs", nature: additive }
  - { spec: "012-store-schema-and-repositories", unit: "crates/aicortex-store/src/lib.rs", nature: additive }
  - { spec: "010-chassis-adoption-and-workspace", unit: "Cargo.toml", nature: additive }
  - { spec: "014-memory-lifecycle-and-erasure", unit: "crates/aicortex-store/src/erasure.rs", nature: additive }
  - { spec: "012-store-schema-and-repositories", unit: "apps/aicortex/tests/migrate.rs", nature: additive }
  - { spec: "016-vector-index-and-scan", unit: "crates/aicortex-index/tests/text.rs", nature: additive }
  - { spec: "014-memory-lifecycle-and-erasure", unit: "crates/aicortex-store/tests/lifecycle.rs", nature: additive }
  - { spec: "015-embedding-pipeline", unit: "crates/aicortex-embed/src/worker.rs", nature: additive }
  - { spec: "010-chassis-adoption-and-workspace", unit: "apps/aicortex/src/cell.rs", nature: additive }
  - { spec: "010-chassis-adoption-and-workspace", unit: "apps/aicortex/Cargo.toml", nature: additive }
  - { spec: "010-chassis-adoption-and-workspace", unit: "apps/aicortex/tests/cell.rs", nature: additive }
  - { spec: "015-embedding-pipeline", unit: "apps/aicortex/tests/embedding_process.rs", nature: additive }
  - { spec: "015-embedding-pipeline", unit: "apps/aicortex/tests/embedding.rs", nature: additive }
  - { spec: "020-http-api-and-scopes", unit: "crates/aicortex-api/src/entities.rs", nature: additive }
  - { spec: "020-http-api-and-scopes", unit: "crates/aicortex-api/src/router.rs", nature: additive }
  - { spec: "020-http-api-and-scopes", unit: "crates/aicortex-api/Cargo.toml", nature: additive }
summary: >
  Similarity alone answers "what looks like this", not "what do I know about
  this person". Entities give recall a second axis: people, projects,
  organizations, places, and artifacts, resolved to canonical records with
  aliases, connected by typed edges from a closed vocabulary. Every entity
  and every edge names the memory it was derived from and the extractor
  version that produced it, and a derived assertion never outranks the
  memory it came from. Extraction is a job on the same outbox as embedding,
  so it inherits the retry and dead-letter discipline rather than inventing
  its own.
---

# 017: Entities and typed edges

## 1. Purpose

The predecessor's users built entity wikis, typed reasoning edges,
authorship edges, and provenance chains on top of a flat table, which is
evidence that the flat table was not enough
(`openbrain://valued-capabilities`). Those capabilities belong in the core,
because a graph built by six independent forks is six graphs.

The risk of putting a model-driven extractor in the core is that inferred
structure starts to look like recorded fact. This spec keeps the two
separable at every point: an entity or edge is derived, carries its source
and extractor, and is always weaker evidence than the memory it came from.

## 2. Territory

The `aicortex-graph` crate and the `entity`, `entity_alias`, `edge`, and
`mention` tables. Extends 012's migration list and 015's worker, which
gains an extraction job kind.

## 3. Behavior

- **B-1 (entity).** `Entity { id, scope, kind: EntityKind, canonical:
  String, aliases: Vec<String>, first_seen, last_seen, mention_count }`.
  `EntityKind` is closed: `Person`, `Organization`, `Project`, `Place`,
  `Artifact`, `Concept`, `Event`. An entity belongs to exactly one scope.
- **B-2 (resolution).** A mention resolves to an entity by exact alias
  match, then by normalized-form match, then by embedding similarity above
  a high threshold within the same kind. Below the threshold a new entity
  is created. Resolution is never destructive: merging two entities is an
  explicit operation that records both prior ids and is reversible for a
  retention window.
- **B-3 (mention).** `mention(memory_id, entity_id, byte range, confidence,
  extractor)` links a memory to an entity at a position, so recall can show
  why an entity matched.
- **B-4 (edge).** `Edge { id, scope, subject: EntityId, predicate:
  Predicate, object: EntityRef, derived_from: Vec<MemoryId>, confidence,
  extractor, valid_from, valid_until, status }`. `EntityRef` is another
  entity or a literal.
- **B-5 (closed vocabulary).** `Predicate` is a closed enum with a stable
  string form: `works_on`, `works_at`, `member_of`, `located_in`,
  `authored`, `depends_on`, `part_of`, `related_to`, `prefers`,
  `decided`, `caused`, `supersedes`. An extractor that proposes an unknown
  predicate has its edge rejected and counted, not stored as free text. A
  new predicate is a migration and a spec amendment, which is what keeps
  the graph queryable.
- **B-6 (derived, never authoritative).** Every edge and entity records
  `derived_from` and `extractor`. Retrieval treats an edge as a routing
  hint that surfaces memories, never as an answer in itself: the result of
  a graph walk is a set of memories with the path that reached them, and
  the memories carry their own trust class (constitution IX).
- **B-7 (extraction as a job).** Extraction runs on the outbox alongside
  embedding, idempotent per `(memory_id, extractor_version)`, with the same
  backoff and dead-letter behavior. Re-extraction with a new version
  supersedes the prior version's edges for that memory rather than
  duplicating them.
- **B-8 (the extractor).** The default extractor is deterministic and
  local: pattern and gazetteer based for people and projects seen before,
  plus the entities already known in the scope. A model-based extractor is
  a configured alternative that runs through the governed egress facade and
  is subject to the same manifest ceiling as remote embedding. Its output
  passes the same vocabulary check and the same confidence floor.
- **B-9 (walk).** `neighbors(entity, depth, filter) -> Vec<(MemoryId,
  Path)>` with a bounded depth (default 2) and a bounded fan-out per hop.
  A walk is always bounded and always returns its path, which becomes part
  of the recall trace.
- **B-10 (contradiction is surfaced, not resolved).** Two edges with the
  same subject and predicate and different objects, both active and
  overlapping in validity, are recorded as a contradiction and surfaced to
  the review queue (023). The system does not silently pick a winner.

## 4. Functional requirements

- **FR-001.** Over the committed extraction fixtures, entities and edges
  match the recorded expectation exactly, including the rejected unknown
  predicate.
- **FR-002.** Two memories mentioning the same person under two aliases
  resolve to one entity, and a third mentioning a different person of the
  same name above the threshold does not.
- **FR-003.** Re-extraction with a bumped version supersedes the prior
  edges for that memory and leaves the count unchanged.
- **FR-004.** A walk of depth 2 from an entity returns memories with a
  path, respects the fan-out bound, and terminates on a cyclic graph.
- **FR-005.** Two contradicting edges produce exactly one contradiction
  record and neither is deleted.
- **FR-006.** Erasing a memory (014 B-7) removes its mentions and reduces
  the `derived_from` of its edges; an edge whose last source is erased is
  marked `origin_erased`.

## 5. Acceptance criteria

- **AC-1.** `cargo test -p aicortex-graph --locked` passes.
- **AC-2.** `aicortex preflight` reports entity and edge counts per scope
  and the extraction dead-letter count.

## 6. Out of scope

Ranking and how graph proximity is weighed against similarity (018). The
review queue that resolves contradictions (023). A general-purpose query
language over the graph, which no consumer needs.

## 7. Resolved decisions

- **D-1 (2026-09-03, this spec).** The predicate vocabulary is closed. A
  free-text predicate is unqueryable in practice: the predecessor's typed
  edge recipes each invented their own strings and no two were joinable.
  Adding a predicate is deliberately a small amendment.
- **D-2 (2026-09-03, this spec).** The default extractor is deterministic
  and local. A model-based default would make every capture an outbound
  call, contradicting 015 D-1 and the privacy boundary.
- **D-3 (2026-11-24, build).** Resolution is scoped to the mention's kind at
  every step, so a `Project` and an `Organization` of one name stay two
  entities. B-2 and FR-002 read together say that a same-name mention is
  the same entity (exact alias) and that a different person is not; the
  build reads "a different person of the same name above the threshold" as
  a different person with a similar name whose context falls below the
  threshold (cosine 0.92, `SIMILARITY_THRESHOLD`). A true homonym with an
  identical surface resolves to one entity and is separated by an explicit
  split, which is out of scope here; a missed merge is cheaper than a wrong
  one.
- **D-4 (2026-11-24, build).** The vector for step three of B-2 is the
  embedding of the chunk covering the mention, at the active model
  revision, read from the table 015 writes; no provider is called. An
  entity stores the vector of its first mention with the model, revision,
  width, and normalization, and a query compares only against the same
  four. A memory without an embedding yet resolves by alias and normalized
  form only.
- **D-5 (2026-11-24, build).** Extraction jobs are staged by a scheduling
  pass over live memories without an `extraction` row at the extractor's
  version, and drained by their own worker, under processor
  `extract.r{version}` in the same Rahi namespace as embedding. The
  embedding worker is not changed, so the declared `extends` edge on
  `crates/aicortex-embed/src/worker.rs` is not exercised by this build; a
  version bump reaches existing memories through the same pass.
- **D-6 (2026-11-24, build).** B-10 is applied literally: two active edges
  with one subject and predicate, different objects, and overlapping
  validity are a contradiction for every predicate. Per-predicate
  cardinality is a later amendment.
- **D-7 (2026-11-24, build).** The tables are created by `aicortex-store`
  (migration 11, not additive), beside 015's and 016's, because erasure
  lives there. `derived_from` is the `edge_source` table. `mention_count`
  is computed, never stored. A merge marks the absorbed entity and every
  read follows the pointer one hop; nothing is moved or deleted, the
  reversal window is 30 days, and an entity that absorbed others cannot
  itself be merged. Confidence is an integer in permille with a floor of
  500, because float arithmetic is denied outside the index and recall
  crates. A superseded edge keeps its source rows; erasing the last of them
  marks it `origin_erased` like any other edge.
- **D-8 (2026-11-24, build).** Landing is split. This change delivers the
  crate, the schema, extraction, resolution, the walk, contradiction
  records, and erasure (FR-001 to FR-006, AC-1). AC-2 (the `app.graph`
  preflight check and the extraction service in the cell) and the
  `GET /entities` routes of 020 D-3 follow in a second change on top of 020,
  so this spec stays `in-progress` until then.

- **D-9 (2026-11-25, build).** The entity routes are mounted under
  `memory.read` like the other reads, and a scope the caller cannot reach is
  the same 404 as everywhere (020 D-4). Entity names come from stored text, so
  both responses carry 019's framing statement beside them and a `derived:
  true` marker; a walk returns memory ids and their paths and never a body,
  which leave only through the envelopes of `/memories`. The listing cursor is
  the last entity id, unsigned, because it only positions a read inside a
  scope the caller already owns. `neighbors` of an id absent from the scope is
  a 404. The wire types live in `entities.rs`, not `dto.rs`, so 020's `dto.rs`
  is untouched; `aicortex-api` gains the dependency on `aicortex-graph`.
- **D-10 (2026-11-25, build).** The cell runs one `extraction-worker` managed
  service, always on, because the default extractor is local and opens no
  socket (D-2). Each pass stages jobs for every scope (`schedule_deployment`,
  100 memories per scope) and drains one batch; it pauses two seconds when
  idle and five after an error, and returns `Ok` only on shutdown. The
  `app.graph` check warns on a dead letter or an open contradiction, and on a
  partial read, and fails only when the store cannot be read. AC-2's report is
  the check's verdict line and its report lines, deployment totals in the
  verdict and one line per scope.

## Status

- **2026-11-25.** `implementation: complete`. FR-001 to FR-006 and AC-1 hold
  from the first change (D-8). AC-2 holds: `aicortex preflight` prints
  `app.graph` with entity and edge counts per scope and the extraction
  dead-letter count (`apps/aicortex/tests/embedding_process.rs` runs the real
  binary; `tests/graph.rs` covers per-scope counts, the dead-letter warning,
  and the extraction service staging, extracting, and returning on shutdown).
  The `GET /entities` routes of 020 D-3 are mounted and tested in
  `crates/aicortex-api/tests/entities.rs`.

## Verification

```verify:cli
cargo test -p aicortex-graph --locked
```
