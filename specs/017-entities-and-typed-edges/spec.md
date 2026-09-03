---
id: "017-entities-and-typed-edges"
title: "Entities and typed edges: a closed predicate vocabulary, derived facts that never outrank their source"
status: approved
kind: "kernel"
domain: "memory"
created: "2026-09-03"
authors: ["Bartek Kus"]
implementation: pending
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
  - "crates/aicortex-graph/tests/resolve.rs"
  - "crates/aicortex-graph/tests/edge.rs"
  - "crates/aicortex-graph/testdata/extraction/"
extends:
  - { spec: "012-store-schema-and-repositories", unit: "crates/aicortex-store/src/migrations.rs", nature: additive }
  - { spec: "015-embedding-pipeline", unit: "crates/aicortex-embed/src/worker.rs", nature: additive }
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

## Verification

```verify:cli
cargo test -p aicortex-graph --locked
```
