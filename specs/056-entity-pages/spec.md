---
id: "056-entity-pages"
title: "Entity pages: one cited, computed view of what the store holds about an entity, with no generated prose"
status: approved
kind: "feature"
domain: "retrieval"
created: "2026-09-27"
authors: ["Bartek Kus"]
implementation: pending
risk: medium
wave: 5
depends_on:
  - "017-entities-and-typed-edges"
  - "019-untrusted-content-boundary"
  - "021-mcp-server"
establishes:
  - "crates/aicortex-graph/src/page.rs"
  - "crates/aicortex-graph/tests/page.rs"
  - "crates/aicortex-graph/testdata/pages/"
extends:
  - { spec: "017-entities-and-typed-edges", unit: "crates/aicortex-graph/src/lib.rs", nature: additive }
  - { spec: "020-http-api-and-scopes", unit: "crates/aicortex-api/src/entities.rs", nature: additive }
  - { spec: "020-http-api-and-scopes", unit: "crates/aicortex-api/src/dto.rs", nature: additive }
  - { spec: "020-http-api-and-scopes", unit: "crates/aicortex-api/tests/api.rs", nature: additive }
  - { spec: "021-mcp-server", unit: "crates/aicortex-mcp/src/tools.rs", nature: additive }
  - { spec: "021-mcp-server", unit: "crates/aicortex-mcp/tests/tools.rs", nature: additive }
references:
  - { unit: { kind: file, path: "docs/design/00-lineage.md" }, role: context }
  - { unit: { kind: file, path: "specs/017-entities-and-typed-edges/spec.md" }, role: constraint }
  - { unit: { kind: file, path: "specs/019-untrusted-content-boundary/spec.md" }, role: constraint }
obligations:
  - { id: "I-1", kind: invariant, text: "An entity page is computed on read from stored entities, edges, mentions, and memories; no page body is persisted and no model generates page text.", anchor: "3-1-a-projection-not-a-document" }
  - { id: "I-2", kind: invariant, text: "Every fact on a page cites at least one source memory that still exists in the scope; a fact whose sources are all erased or superseded does not appear.", anchor: "3-2-what-a-page-holds" }
  - { id: "I-3", kind: invariant, text: "Every memory body and every literal edge object on a page is returned inside the 019 envelope.", anchor: "3-3-trust-and-the-boundary" }
  - { id: "R-1", kind: requirement, text: "The page is served by GET /entities/{id} and by the existing relate MCP tool; the MCP surface stays at seven tools.", anchor: "3-4-surfaces" }
summary: >
  The predecessor's users built entity wikis by hand on top of a flat table
  (openbrain://valued-capabilities). Spec 017 supplies the entities, aliases,
  typed edges, and mentions, and a bounded neighbor walk, but no single view
  of what the store holds about one entity. This spec adds that view as a
  pure, bounded projection computed on read: header, facts grouped by
  predicate with their source memories, open contradictions shown side by
  side, and recent mentions in 019 envelopes. It generates no prose, stores
  no page, and so needs no erasure path of its own.
---

# 056: Entity pages

## 1. Purpose

The lineage records entity wikis among the capabilities the predecessor's
users actually built (`docs/design/00-lineage.md`,
`openbrain://valued-capabilities`), and spec 017 opens by saying that
capability belongs in the core "because a graph built by six independent
forks is six graphs". 017 then delivers the graph and a neighbor walk, and
020 and 021 expose `GET /entities` and `relate` over it. What no approved
spec delivers is the page: the one answer to "what do I know about this
person or project, and why do I believe it".

A wiki written by a model would be the wrong way to supply it. Generated
summary text is derived content that reads like recorded fact, which is
exactly what 017 B-6 forbids, and a stored page is one more copy of memory
content that erasure has to find (constitution, "forgetting is real"). The
page this spec defines is therefore a projection: structured, computed on
every read, every line tied to the memories it came from.

## 2. Territory

A `page` module in `aicortex-graph` (017) with its tests and fixtures. It
extends 020's entities handler and DTOs with `GET /entities/{id}`, and 021's
`relate` tool with a page view. It adds no table, no migration, no worker,
and no tool.

## 3. Behavior

### 3.1 A projection, not a document

- **B-1 (computed on read).** `entity_page(scope, entity_id, as_of,
  bounds) -> EntityPage` MUST be a pure function of the stored entity,
  aliases, edges, mentions, and memories visible at `as_of`. It MUST NOT
  persist its result, cache it across requests, call a model, or emit
  prose that is not a stored value or a fixed label authored in this
  crate (I-1).
- **B-2 (deterministic).** For the same store state and arguments the page
  MUST be byte-identical, including order. Ordering rules are B-5's.

### 3.2 What a page holds

- **B-3 (sections).** A page has exactly these sections:
  1. **Header:** canonical name, kind, aliases, first and last seen, and
     mention count (017 B-1), plus the prior ids of any merge into this
     entity (017 B-2).
  2. **Facts:** active edges with this entity as subject or object,
     grouped by predicate in 017 B-5's vocabulary order. Each fact carries
     the other end, confidence, extractor and version, validity interval,
     and the ids of the memories in `derived_from`.
  3. **Contradictions:** each 017 B-10 contradiction that involves this
     entity, with every side shown and the review item it is waiting on
     (023). The page never picks a winner.
  4. **Mentions:** the most recent memories that mention the entity, each
     with the byte range of the mention (017 B-3).
- **B-4 (cited or absent).** A fact MUST cite at least one memory in the
  scope that is neither erased nor superseded at `as_of`. A fact whose
  every source is gone MUST NOT appear, even if the edge row has not yet
  been removed by 017's propagation (I-2). This makes the page correct
  during the window between an erasure and the edge cleanup.
- **B-5 (order).** Within a predicate, facts order by the strongest trust
  class among their live sources, then by confidence, then by edge id.
  Mentions order by memory `created_at` descending, then id. A derived
  edge never sorts above a fact of the same predicate whose source memory
  has a higher trust class (017 B-6).
- **B-6 (bounded).** Facts per predicate, contradictions, and mentions each
  have a default and a maximum bound, and each truncated section returns a
  keyset continuation token. A page read never scans an unbounded set.

### 3.3 Trust and the boundary

- **B-7 (envelopes).** Every memory body and every literal edge object
  (017 B-4 `EntityRef` literal, which is extracted content) MUST be
  returned inside 019's envelope with its origin and trust class (I-3).
  Entity names and aliases are labelled as derived values.
- **B-8 (no trust from assembly).** Appearing on a page grants nothing: no
  fact, mention, or header value gains a trust class by being gathered
  here, and the page carries 019's framing statement.

### 3.4 Surfaces

- **B-9 (HTTP).** `GET /entities/{id}` returns the page under
  `memory.read`, with `as_of` and section bounds as query parameters. An
  entity outside the caller's scope answers exactly as an absent one does
  (020's non-disclosing 404).
- **B-10 (MCP).** `relate` accepts an optional `view` of `neighbors`
  (the default and 021's current behavior) or `page`. The page view
  returns the same structure as structured content. The MCP surface stays
  at seven tools (021 B-5), and the tool description remains static text
  (021 B-6).

## 4. Functional requirements

- **FR-001.** A fixture scope with an entity, two predicates, a merge, a
  contradiction, and mentions produces the committed page in
  `testdata/pages/` byte for byte.
- **FR-002.** After erasing the only memory behind a fact, the page omits
  the fact before and after 017's propagation runs.
- **FR-003.** A hostile literal edge object and a hostile memory body
  (019's delimiter fixtures) appear only inside envelopes.
- **FR-004.** Truncated sections return continuation tokens that resume
  without gaps or duplicates.
- **FR-005.** A cross-scope `GET /entities/{id}` is indistinguishable from
  an unknown id, and `relate` with `view: "page"` in the tool fixtures
  matches the HTTP page.
- **FR-006.** A test asserts the crate performs no write and no egress
  while building a page (the store handle is read-only and the kernel
  facade records no call).

## 5. Acceptance criteria

- **AC-1.** `cargo test -p aicortex-graph --locked --test page` passes.
- **AC-2.** `cargo test -p aicortex-api --locked --test api` passes with
  the entity page cases.
- **AC-3.** `cargo test -p aicortex-mcp --locked --test tools` passes with
  the page view and still counts seven tools.

## 6. Out of scope

Model-written summaries of an entity, which would be an egress addition
(041) and would violate B-1. Editing an entity through its page; merges and
corrections stay 017's and 023's operations. Pages for 050's claim
subjects, whose subjects are host-domain identifiers rather than 017
entities; joining them is a later decision. A rendered HTML wiki, which is
a client concern.

## 7. Resolved decisions

None yet. Open for the owner before approval:

- **Q-1.** Whether the page should also list the entity's 1-hop
  neighbors as entity references (names only, no bodies), or leave that to
  `relate` with `view: "neighbors"`.

## Verification

```verify:cli
cargo test -p aicortex-graph --locked --test page
cargo test -p aicortex-api --locked --test api
cargo test -p aicortex-mcp --locked --test tools
```
