---
id: "023-review-and-promotion"
title: "Review: the queue where quarantine, contradiction, and promotion wait for a human"
status: approved
kind: "feature"
domain: "memory"
created: "2026-09-03"
authors: ["Bartek Kus"]
implementation: in-progress
risk: high
wave: 2
depends_on:
  - "022-client-adapters"
establishes:
  - "crates/aicortex-curate/Cargo.toml"
  - "crates/aicortex-curate/src/lib.rs"
  - "crates/aicortex-curate/src/review.rs"
  - "crates/aicortex-curate/src/promotion.rs"
  - "crates/aicortex-curate/src/migrations.rs"
  - "crates/aicortex-curate/tests/review.rs"
  - "crates/aicortex-curate/tests/common/mod.rs"
  - "crates/aicortex-store/src/review_tables.rs"
extends:
  - { spec: "012-store-schema-and-repositories", unit: "crates/aicortex-store/src/migrations.rs", nature: additive }
  - { spec: "012-store-schema-and-repositories", unit: "crates/aicortex-store/src/lib.rs", nature: additive }
  - { spec: "018-retrieval-and-recall-trace", unit: "crates/aicortex-store/tests/recall_trace.rs", nature: additive }
  - { spec: "020-http-api-and-scopes", unit: "crates/aicortex-api/src/router.rs", nature: additive }
references:
  - { unit: { kind: file, path: "specs/019-untrusted-content-boundary/spec.md" }, role: constraint }
summary: >
  Three situations need a person: content whose origin could not be
  established and was quarantined, two edges that contradict each other, and
  a memory that someone wants a client to act on. This spec is the one queue
  where all three wait, the decisions that empty it, and the ledger records
  those decisions produce. Promotion is the important one: it is the only
  path to instruction grade, it requires the promote scope and an
  interactive principal, and it writes a rahi Decision whose id becomes part
  of the memory's trust.
---

# 023: Review and promotion

## 1. Purpose

Constitution X says a client may act only on memories a human promoted.
That is a policy until there is a place the human does it. This spec is
that place, and it deliberately batches the three human decisions the
system generates rather than inventing a surface per decision.

## 2. Territory

The `aicortex-curate` crate is founded here with the review queue and
promotion. Its curation workers arrive in 034. The queue's routes are added
to the API router.

## 3. Behavior

- **B-1 (the queue).** `review_item(id, scope, kind: ReviewKind, subject,
  created, state, resolved_by, decision_id)` where `ReviewKind` is
  `Quarantine`, `Contradiction`, `PromotionRequest`, or `GateOverride`.
  Items are created by the gate (013 B-7), the graph (017 B-10), the API,
  and the operator override path.
- **B-2 (routes).** `GET /review` (paged, filtered by kind and state),
  `POST /review/{id}/resolve` with a typed resolution per kind. The queue
  count is exposed as an MCP resource (021 B-8) so an assistant can tell
  the user there is something waiting, without being able to resolve it.
- **B-3 (quarantine resolution).** `Admit` moves the memory to `Active`
  with `trust: Assertion` and records who admitted it; `Reject` erases it
  through 014 B-7. Both write a Decision.
- **B-4 (contradiction resolution).** The resolver picks the surviving
  edge, supersedes the other, or marks both as historically valid with
  disjoint validity ranges. A Decision records the choice and the reason
  text.
- **B-5 (promotion).** `POST /promote/{id}` requires the `memory.promote`
  scope and an interactive principal (`rahi://025` B-4 refuses a
  client-credentials token). It appends a rahi Decision, receives the
  `DecisionId`, constructs the `Promotion` token, and writes the memory's
  trust class as `Instruction` with the decision id stored beside it. The
  `Promotion` type is the only bridge, and it cannot be produced any other
  way (011 B-5).
- **B-6 (demotion).** `POST /demote/{id}` returns a memory to `Assertion`
  and writes a Decision. Promotion is reversible; the ledger keeps both
  records.
- **B-7 (expiry of promotion).** A promotion may carry a review interval.
  When it lapses the memory returns to `Assertion` automatically and a
  review item is created, because a standing instruction nobody has
  reconfirmed in a year is not a standing instruction.
- **B-8 (no agent path).** Every route here requires an interactive
  principal. There is no MCP tool, no service-callable route, and no
  configuration that grants promotion to an automated client.
- **B-9 (envelopes).** Review items display memory content, so responses
  obey 019.

## 4. Functional requirements

- **FR-001.** A quarantined memory becomes retrievable only after an
  `Admit` resolution, and its trust class is `Assertion`, never higher.
- **FR-002.** A promotion by an interactive principal writes exactly one
  Decision, and the memory's trust class is `Instruction` with that
  decision id.
- **FR-003.** A promotion attempt by a client-credentials token is refused
  with 403 and writes no Decision that suggests success.
- **FR-004.** A lapsed promotion returns the memory to `Assertion` and
  creates a review item, driven by an injected clock.
- **FR-005.** Resolving a contradiction leaves exactly one active edge for
  the subject and predicate, or two with disjoint validity.

## 5. Acceptance criteria

- **AC-1.** `cargo test -p aicortex-curate --locked --test review` passes.
- **AC-2.** `aicortex ledger verify` succeeds after a promotion, a
  demotion, and a quarantine rejection.

## 6. Out of scope

Automated curation (034). A review user interface, which is a later
concern; the routes and the queue count are the surface for now.

## 7. Resolved decisions

- **D-1 (2026-09-03, this spec).** Promotion expires by default with a
  configurable interval, rather than lasting forever. A memory system's
  worst failure mode is a stale instruction, and the cost of reconfirmation
  is one queue item.
- **D-2 (2026-11-26, build, crate layout).** `aicortex-curate` is a library
  over the store and the ledger: it has no HTTP, axum, or recall dependency.
  The routes of B-2, B-5, and B-6 are handlers in `aicortex-api`, which takes
  the curate crate as a dependency (020 D-20) and frames every body it returns
  through 019's envelope. The dependency table in `CLAUDE.md` lists curate
  above api; that ordering describes the curation workers of 034, which
  consume the API's surface. The review queue itself must sit below the
  routes that mount it, because spec 020's router is where B-2 says its
  routes are added. The manifest of the new crate has only workspace and path
  dependencies, so it lands with the manifest commit of this change.
- **D-3 (2026-11-26, build, migration 14).** The review tables are migration
  14 (`REVIEW_VERSION`, additive, in `crates/aicortex-store/src/review_tables.rs`
  beside 015's, 016's and 017's tables), because the migration list is the
  store's and this crate depends on the store. 12 (025's conversation
  reference) and 13 (018's recall traces) landed ahead of it, and 053 D-9
  requires the host set's versions to be contiguous (`host_library.rs` and the
  external-host fixture assert it), so 14 is the next free version and a gap
  would fail four tests. Whoever merges after another renumbers:
  `REVIEW_VERSION` is the one constant that moves, and no test names the
  number. The tests that said "the newest migration is the graph's" or
  listed the versions already follow the list, and 018's `recall_trace.rs`
  now asks only that the trace table be no newer than the expected version. The tables hold ids, labels,
  times and the Decision id, never a body: `review_item` and `promotion`.
- **D-4 (2026-11-26, build, how the queue is filled).** B-1 lists the gate,
  the graph, the API and the operator path as producers. The gate and the
  graph already leave durable rows (a `quarantined` memory, an open
  `contradiction`), and so does the embedding worker when it quarantines in
  flight, so the queue is filled by reconciling against those rows
  (`ReviewQueue::collect`) on the routes that read it, not by editing three
  other specs' write paths. Reconciliation is idempotent (a partial unique
  index on open items), opens at most 200 items per source per pass, and
  withdraws an open item whose subject stopped needing a person (a memory
  erased elsewhere). 056 D-5's reading, that the contradiction record is the
  review item until 023 lands, is what collection implements.
  `PromotionRequest` is opened by `POST /review` (a person's token with
  `memory.write`; deciding needs `memory.promote`) and by a lapse (B-7).
  `GateOverride` exists in the vocabulary and resolves by `acknowledge`, but
  no producer opens one: the operator override path is not specified by 013 and
  is left to the spec that defines it.
- **D-5 (2026-11-26, build, Decisions).** Every resolution appends exactly one
  Decision first and does what it says second, in one transaction that also
  closes the item; a failed act appends a refusing Decision under a new id so
  the chain never shows a success that did not happen (FR-003's rule applied
  to a lost race). Kinds: `memory.promote`, `memory.demote`,
  `memory.promotion_lapsed`, `review.admit`, `review.contradiction`,
  `review.decline`, `review.acknowledge`; a rejection is 014's own
  `memory.erase`. Payloads name ids, the resolution label, and a reviewer's
  bounded reason (1 to 500 characters), and never content. A lapse has no
  person acting, and every Decision names an actor, so the lapse names the
  subject whose promotion it ends. The Decision references beyond these (024)
  are not built here.
- **D-7 (2026-11-26, build, contradiction resolution and 019 FR-005).** B-4's
  three choices are two resolutions: `keep_edge` (the named edge stays active,
  the other becomes `superseded` with `valid_until` set to the later of its
  own start and the resolution time) and `disjoint` (the earlier edge ends at
  a boundary the reviewer names and the later begins there; the boundary must
  lie inside the period both edges claimed, so the edges stop overlapping and
  017's overlap test stays silent). The review views are the only derived
  presentation this spec emits, and each is labelled with the lowest trust
  class of the memories it shows (019 B-5), so an `Assertion` memory's item is
  an `Assertion`.
  019 FR-005's curator digest is 034's to generate and inherits the same
  rule; this spec generates no digest of its own.
- **D-8 (2026-11-26, build, promotion expiry).** A promotion carries a review
  interval of 1 hour to 10 years, one year by default (D-1), and there is no
  "never". `Promoter::expire_due` takes the clock as an argument and is
  bounded to 100 promotions per scope per pass. Driving it on a schedule is a
  managed service in the cell, which is left to the curation workers of 034
  and the cell's service list; no cell file is changed by this part.

## Status

- **2026-11-26.** `implementation: in-progress`. The curate crate, the review
  tables and the promotion rules of this spec hold in
  `cargo test -p aicortex-curate --locked --test review`. The routes and the
  MCP count follow in a later part of this build.

## Verification

```verify:cli
cargo test -p aicortex-curate --locked --test review
```
