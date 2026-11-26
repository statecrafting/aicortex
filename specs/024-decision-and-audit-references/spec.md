---
id: "024-decision-and-audit-references"
title: "One chain, referenced: decisions live in rahi's ledger and memories point at them"
status: approved
kind: "feature"
domain: "memory"
created: "2026-09-03"
authors: ["Bartek Kus"]
implementation: in-progress
risk: high
wave: 2
depends_on:
  - "023-review-and-promotion"
establishes:
  - "crates/aicortex-store/src/audit.rs"
  - "crates/aicortex-store/src/decision_ref.rs"
  - "crates/aicortex-store/tests/audit.rs"
  - "crates/aicortex-curate/tests/history.rs"
extends:
  - { spec: "012-store-schema-and-repositories", unit: "crates/aicortex-store/src/lib.rs", nature: additive }
  - { spec: "012-store-schema-and-repositories", unit: "crates/aicortex-store/src/migrations.rs", nature: additive }
  - { spec: "014-memory-lifecycle-and-erasure", unit: "crates/aicortex-store/src/erasure.rs", nature: additive }
  - { spec: "020-http-api-and-scopes", unit: "crates/aicortex-api/src/capture.rs", nature: additive }
  - { spec: "020-http-api-and-scopes", unit: "crates/aicortex-api/src/memories.rs", nature: additive }
  - { spec: "020-http-api-and-scopes", unit: "crates/aicortex-api/src/router.rs", nature: additive }
  - { spec: "023-review-and-promotion", unit: "crates/aicortex-curate/src/review.rs", nature: additive }
  - { spec: "023-review-and-promotion", unit: "crates/aicortex-curate/src/promotion.rs", nature: additive }
constrains:
  - { flavor: invariant-freeze, unit: "crates/aicortex-store/src/decision_ref.rs", note: "one chain per deployment; this product appends, never chains" }
summary: >
  There is exactly one hash chain in a deployment and it belongs to the
  chassis. This product appends Decisions to it and stores references to
  them; it never builds a second chain, and it never puts memory content
  into the one that exists. The audit view is a join: a memory's history is
  its lifecycle rows plus the decisions that reference it, rendered in one
  timeline. This also fixes the vocabulary, because the sibling product in
  this family has its own object called a ledger and two things called
  ledger in one deployment would be a permanent source of confusion.
---

# 024: Decisions and audit

## 1. Purpose

Three specs already append Decisions: the gate on refusal, the review queue
on resolution, and erasure. Without one place that says how, each would
invent its own record shape and its own retention story, and one of them
would eventually put a memory body into the chain, which would make erasure
impossible to honor (constitution XIII).

This spec also draws a boundary that matters across the family. `hqgit`'s
central object is a per-repository evidence DAG that it calls a ledger. The
chassis's decision chain is a different thing: an operational record of
governed choices in one deployment. This product uses the second and never
the first, and says so in its own vocabulary so that a shared deployment
cannot confuse them.

## 2. Territory

Two modules and one test in `aicortex-store`, plus a `decision_ref` table.

## 3. Behavior

- **B-1 (one chain).** Every Decision this product writes goes to rahi's
  ledger through its API. This repository contains no hash-linking, no
  parent-hash column, and no verification routine of its own; a grep test
  asserts it.
- **B-2 (the record shape).** A Decision carries the capability, the actor
  subject, the scope id, the affected memory or entity ids, a reason code
  from a closed enum, and an optional short justification supplied by a
  human. It never carries a memory body, a chunk, an embedding, or a
  reconstructible hash preimage of content.
- **B-3 (references).** `decision_ref(memory_id, decision_id, kind,
  created)` records the link in the application store, so a memory's
  audit history is a bounded indexed read rather than a scan of the chain.
  The chain remains the authority; the table is an index into it.
- **B-4 (the audit view).** `history(scope, memory_id)` returns a merged
  timeline: creation, merges, supersessions, corrections, promotions,
  demotions, quarantine transitions, and erasure, each with its decision
  id where one exists, ordered by time. It is served by the API under
  `GET /memories/{id}/history`.
- **B-5 (erasure and the chain).** An erased memory keeps its
  `decision_ref` rows and its tombstone, so the history remains readable
  and says what happened, while the content is gone. The chain is never
  rewritten, and `ledger verify` is unaffected by erasure.
- **B-6 (vocabulary).** In this repository the word ledger always means
  rahi's decision chain. The application never uses it for anything else,
  and documentation that mentions the sibling product's evidence DAG names
  it explicitly.
- **B-7 (no blocking append).** The request path never awaits a ledger
  append; failures are retried by the outbox and surfaced by preflight and
  metrics, per the chassis's discipline.

## 4. Functional requirements

- **FR-001.** A grep test asserts no parent-hash column, no chain
  verification, and no signing key handling exists in this workspace.
- **FR-002.** Every Decision emitted by the gate, the review queue, and
  erasure validates against the closed reason-code enum.
- **FR-003.** A Decision serialized from an erasure contains no substring
  of the erased body.
- **FR-004.** `history` for a memory with ten lifecycle events issues a
  bounded number of statements independent of the chain's length.
- **FR-005.** `aicortex ledger verify` passes after a suite that exercises
  every Decision-emitting path.

## 5. Acceptance criteria

- **AC-1.** `cargo test -p aicortex-store --locked --test audit` passes.
- **AC-2.** `GET /memories/{id}/history` returns the full timeline for a
  memory that was captured, merged, corrected, promoted, and erased.

## 6. Out of scope

The chain's implementation, sealing, and archive, which are the chassis's
(`rahi://013`, `rahi://014`). Cross-deployment attestation, which belongs
to the sibling product.

## 7. Resolved decisions

- **D-1 (2026-09-03, this spec).** A `decision_ref` index table rather
  than querying the chain by content. The chain is append-only and grows
  without bound, is sealed and archived, and is not indexed by application
  concerns; an index in the application store keeps the audit view cheap
  while leaving the chain authoritative.
- **D-2 (2026-11-26, build, migration 15).** The `decision_ref` table is
  migration 15, the next free version after 023's review tables (14), and is
  additive: an older binary never reads it. `EXPECTED_SCHEMA_VERSION` follows
  it. The table is keyed `(scope_id, memory_id, decision_id)` and its `kind`
  column is a closed set (`quarantine`, `review_admit`, `review_decline`,
  `review_acknowledge`, `promote`, `demote`, `lapse`, `erase`), which is what
  the history names an event by. A replay of the same Decision against the
  same memory is the same row.
- **D-3 (2026-11-26, build, the closed reason-code set).** B-2's enum is
  `ReasonCode` in `decision_ref.rs`: the gate's seven codes, the erasure
  codes (`subject_request`, `user_request`, `retention_policy`,
  `legal_hold_lift`, `review_reject`), the review and promotion codes, and
  `act_not_committed` for the refusing Decision that follows an act that did
  not commit. `append_decision` validates before it appends, and every
  emission site of the gate, the review queue and promotion calls it. Erasure
  validates the Decision it builds, in both its memory and scope forms, before
  it stages anything, so an `Authority::because` code outside the set makes the
  erasure refuse. A payload names a `scope`, a `reason_code`, no key that
  names content at any depth, and an optional `justification` of at most 280
  bytes. The reviewer's `reason` text on a review Decision predates this spec
  and is bounded by 023's own limit; it is not renamed here.
- **D-4 (2026-11-26, build, references commit with the act).** A reference is
  staged into the transaction that carries the act it records: the gate's
  quarantine with the memory row, review admit, decline and acknowledge with
  the item's close, promotion, demotion and lapse with the memory update, and
  erasure with the tombstone and the receipt. The gate's Decision id is
  derived from the candidate, so the reference is known before the Decision is
  appended; a failed append after the commit leaves an index row for a Decision
  the chain does not hold, which the chain, as the authority, resolves. Review
  decline and acknowledge now append the refusing Decision on a failed commit,
  as admit already did. A scope erasure writes no per-memory references,
  because its Decision names the scope.
- **D-5 (2026-11-26, build, what an erased history keeps).** B-5 keeps the
  tombstone and the references. Erasure also removes the memory's source log
  and derivation rows (014 B-7), so an erased memory's history lists its
  creation, the Decisions that referenced it, and the erasure, and no longer
  its merges or the memory it corrected. The history is one bounded statement
  over the memory row, the source log, the derivation rows and
  `decision_ref`, capped at 500 events with a `truncated` flag, and it reports
  how many statements it issued (FR-004).
- **D-6 (2026-11-26, build, where the route and its proof live).**
  `GET /memories/{id}/history` is in `aicortex-api`'s memory routes under
  `memory.read`, with the same scope resolution as `GET /memories/{id}`. AC-2
  needs promotion, which is 023's and sits above the API, so its test is
  `aicortex-curate/tests/history.rs`, which also runs the gate refusal, the
  review, promotion and erasure paths through FR-002 and FR-005. The gate's
  quarantine path is not reachable through the API (a capture's origin is
  always authenticated there), so its reference is staged by the same code
  but not exercised end to end.
- **D-7 (2026-11-26, build, B-7 is not delivered here).** B-7 asks that the
  request path never await a ledger append and that failures be retried.
  This build keeps each path's existing order (the gate and erasure append
  after the act commits; review and promotion append before it and refuse on
  failure) and adds no retry for Decisions. B-7 therefore remains open.

## Verification

```verify:cli
cargo test -p aicortex-store --locked --test audit
```
