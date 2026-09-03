---
id: "014-memory-lifecycle-and-erasure"
title: "Lifecycle: supersession, correction, expiry, deduplication, and erasure that reaches the vectors"
status: approved
kind: "kernel"
domain: "memory"
created: "2026-09-03"
authors: ["Bartek Kus"]
implementation: pending
risk: critical
wave: 1
depends_on:
  - "013-write-gate-and-redaction"
establishes:
  - "crates/aicortex-store/src/lifecycle.rs"
  - "crates/aicortex-store/src/fingerprint.rs"
  - "crates/aicortex-store/src/erasure.rs"
  - "crates/aicortex-store/tests/lifecycle.rs"
  - "crates/aicortex-store/tests/erasure.rs"
extends:
  - { spec: "012-store-schema-and-repositories", unit: "crates/aicortex-store/src/lib.rs", nature: additive }
  - { spec: "012-store-schema-and-repositories", unit: "crates/aicortex-store/src/migrations.rs", nature: additive }
constrains:
  - { flavor: invariant-freeze, unit: "crates/aicortex-store/src/erasure.rs", note: "erasure reaches every derivative; memory content never enters the ledger" }
summary: >
  A memory store that can only append becomes wrong over time and then
  confidently recalls the wrong thing. This spec gives a memory the rest of
  its life: an exact fingerprint that merges a repeated capture instead of
  duplicating it, supersession that keeps the old row readable but
  unretrievable, correction that records who said the old value was wrong,
  expiry for memories that were true until a date, and erasure that removes
  content, chunks, embeddings, and index entries in one transaction while
  the ledger records only that it happened.
---

# 014: Lifecycle

## 1. Purpose

The predecessor deduplicated on exact normalized text and had no other
lifecycle at all, so near-duplicates accumulated without bound and a fact
that stopped being true stayed retrievable forever
(`openbrain://exact-dedup-only`). Confidently recalling a stale fact is the
characteristic failure of memory systems and the one users notice.

This spec also resolves the tension constitution XIII names: the decision
chain is append-only and hash-linked, and a user has a right to erasure.
Both are satisfiable at once, provided content never enters the chain.

## 2. Territory

Three modules and two tests inside `aicortex-store`, plus the migrations
they add. Extends 012's `lib.rs` and migration list.

## 3. Behavior

- **B-1 (fingerprint).** `fingerprint(scope, normalized_text) -> [u8; 32]`,
  a BLAKE3 hash over the gate-normalized body with the scope mixed in.
  Unique among non-erased rows per 012 B-2.
- **B-2 (merge on repeat).** A capture whose fingerprint already exists in
  the scope does not insert. It merges: `updated` moves forward,
  `importance.uses` increments, the new provenance is appended as an
  additional source row, and any new metadata is unioned. The caller is
  told it was a merge and receives the existing id. This makes a repeated
  import idempotent by construction.
- **B-3 (supersession).** `supersede(old, new)` sets the old row to
  `Status::Superseded(new)` in the same transaction that inserts the new
  one. Superseded rows are excluded from retrieval, retained for audit,
  and readable by id. A supersession chain is walkable in both directions
  and is asserted acyclic on write.
- **B-4 (correction).** A `Correction` memory records that a prior memory
  was wrong, names it in `derived_from`, and supersedes it. The correction
  carries the actor who made it. A correction by an agent is an
  `Assertion`; only a human correction can be promoted further.
- **B-5 (expiry).** A memory may carry `valid_until`. A curator pass moves
  expired rows to `Status::Expired` in bounded batches under a rahi lease
  with its fencing token. Expiry is a status change, never a delete.
- **B-6 (decay is read-time).** Importance decays by computation at read
  time (011 B-9). No background job rewrites rows to decay them, because
  that would be a write amplification proportional to the store size on
  every tick.
- **B-7 (erasure).** `erase(scope, id, authority)` deletes, in one
  transaction: the body, every chunk, every embedding, every index entry,
  every derivation row naming it as a child, and its provenance detail. It
  retains the row shell with `Status::Erased`, the id, the scope, and the
  timestamps, so that references from other memories resolve to a
  tombstone rather than dangling. It appends one ledger Decision naming
  the scope, the id, the authority, and a count of removed derivatives,
  and never the content or its hash preimage.
- **B-8 (erasure of derived memories).** Erasing a memory does not
  automatically erase memories derived from it, because a derived summary
  may be independently valuable, but it marks each derivative
  `origin_erased` and makes that visible in recall traces. Cascading
  erasure is an explicit flag on the call and is reported in the Decision.
- **B-9 (scope erasure).** `erase_scope` is the same operation over every
  memory in a scope, run in bounded batches under a lease, resumable, and
  reported by a single Decision at completion with the per-batch progress
  in the work journal.
- **B-10 (near-duplicates are not handled here).** Semantic deduplication
  requires embeddings and is a curator concern (034). This spec handles
  exact repeats only, and says so rather than half-solving it.

## 4. Functional requirements

- **FR-001.** Capturing the same text twice in one scope yields one row,
  two provenance rows, and `uses == 2`; capturing it in another scope
  yields a separate row.
- **FR-002.** A superseded memory is absent from a retrieval result and
  present in a by-id read; the chain from new to old is walkable.
- **FR-003.** After `erase`, a targeted search that previously returned
  the memory returns nothing, and a direct read of the embedding and chunk
  tables for that id returns zero rows.
- **FR-004.** The ledger Decision emitted by `erase` contains no substring
  of the erased body, asserted over a fixture whose body is a distinctive
  token.
- **FR-005.** `erase_scope` over 5000 memories completes in bounded
  batches, is resumable after an induced crash, and leaves no orphaned
  embedding or chunk row.
- **FR-006.** A supersession that would create a cycle is rejected.

## 5. Acceptance criteria

- **AC-1.** `cargo test -p aicortex-store --locked --test lifecycle` and
  `--test erasure` pass.
- **AC-2.** An erasure followed by `aicortex ledger verify` leaves the
  chain intact.

## 6. Out of scope

Semantic near-duplicate consolidation and summarization (034). The review
queue that promotes out of quarantine (023). Retention policy defaults,
which are deployment configuration.

## 7. Resolved decisions

- **D-1 (2026-09-03, this spec).** Erasure retains a tombstone row rather
  than deleting it outright. A hard delete breaks referential integrity
  from derived memories and from recall traces, and produces a system that
  cannot explain why a citation is missing. The tombstone carries no
  content, which is what the erasure right requires.

## Verification

```verify:cli
cargo test -p aicortex-store --locked --test lifecycle
cargo test -p aicortex-store --locked --test erasure
```
