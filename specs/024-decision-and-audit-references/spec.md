---
id: "024-decision-and-audit-references"
title: "One chain, referenced: decisions live in rahi's ledger and memories point at them"
status: approved
kind: "feature"
domain: "memory"
created: "2026-09-03"
authors: ["Bartek Kus"]
implementation: pending
risk: high
wave: 2
depends_on:
  - "023-review-and-promotion"
establishes:
  - "crates/aicortex-store/src/audit.rs"
  - "crates/aicortex-store/src/decision_ref.rs"
  - "crates/aicortex-store/tests/audit.rs"
extends:
  - { spec: "012-store-schema-and-repositories", unit: "crates/aicortex-store/src/lib.rs", nature: additive }
  - { spec: "012-store-schema-and-repositories", unit: "crates/aicortex-store/src/migrations.rs", nature: additive }
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

## Verification

```verify:cli
cargo test -p aicortex-store --locked --test audit
```
