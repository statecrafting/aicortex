---
id: "014-memory-lifecycle-and-erasure"
title: "Lifecycle: supersession, correction, expiry, deduplication, and erasure that reaches the vectors"
status: approved
kind: "kernel"
domain: "memory"
created: "2026-09-03"
authors: ["Bartek Kus"]
implementation: in-progress
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
  - { flavor: invariant-freeze, unit: "crates/aicortex-store/src/erasure.rs", note: "erasure destroys the B-9 digest key of every Decision about the erased memory or scope" }
references:
  - { unit: { kind: file, path: "specs/013-write-gate-and-redaction/spec.md" }, role: constraint }
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

This spec also carries spec 013's erasure obligation, because `erasure.rs`
is this spec's file to write. Spec 013 B-9 mints a keyed digest per
Decision and holds the key in an application-store row; the property that
erasure destroys those keys is frozen here, on the unit that implements it,
and is discharged by B-11 and FR-007 (amended 2026-09-17, D-2). Spec 013
FR-008 forbids any surface or document from claiming that erasure reaches
refusal and quarantine records until FR-007 below shows it, and that
prohibition stays 013's.

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
- **B-11 (digest keys).** Erasure destroys the B-9 digest key of every
  Decision about the erased memory or scope. This is spec 013 B-9's
  requirement, frozen on this spec's unit and carried here verbatim
  (amended 2026-09-17, D-2): erasing a quarantined memory (B-7) destroys
  its Decision's key, and erasing a scope (B-9) destroys every key in the
  scope, in the same transaction as the rest of the erasure. The staging
  calls exist already, as `DecisionKeyRepo::stage_destroy_for_memory` and
  `stage_destroy_for_scope` in the `decision_key.rs` spec 013 established;
  what this spec adds is that `erase` and `erase_scope` call them and that
  FR-007 proves they did. A destroyed key leaves the chained digest an
  opaque value that cannot be confirmed against a guessed body, which is
  what lets an append-only chain and constitution XIII both hold.
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
- **FR-007.** The three things spec 013 FR-008 requires before erasure may
  be claimed to reach refusal and quarantine records, each asserted rather
  than described. After erasing a quarantined memory, and after erasing a
  scope, no `decision_key` row covering the erased object remains and
  `DecisionKeyRepo::get` returns `None` for the key id the Decision names.
  A backup taken after the erasure holds no such key. No replay path
  restores one: neither redriving the outbox, nor re-importing the same
  content, nor a capture of the same body mints a row under the destroyed
  key id. The result of an erasure says in so many words that a backup
  taken *before* it still holds the key until that backup is discarded,
  rather than implying otherwise.

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

- **D-2 (2026-09-17, amendment, human-authorized).** Spec 013 originally
  froze "erasure destroys the B-9 digest key of every Decision about the
  erased memory or scope" on `crates/aicortex-store/src/erasure.rs`, a file
  this spec establishes and which does not exist until this spec is built.
  That forward `constrains` edge made 013 unable to reach
  `implementation: complete` without a blocking `I-004`: the corpus
  contract makes an unresolved *owned* unit an error at the `complete`
  tier, and `constrains` is an owning edge there. The maintainer authorized
  this specific amendment on 2026-09-17: move the constraint to the spec
  that establishes the unit, preserving the requirement and its
  traceability, and explicitly not a general relaxation of `constrains`
  validation.

  The invariant is carried here verbatim, on the same unit, in this spec's
  `constrains` list. What the move adds is enforcement a note by itself
  could not give: B-11 makes the obligation a behavior of this spec, and
  FR-007 makes it evidence this spec's acceptance must produce, so
  `spec-spine verify 014` fails if `erase` and `erase_scope` do not destroy
  the keys. The `references` edge to `specs/013-write-gate-and-redaction/
  spec.md` with `role: constraint` is the corpus's existing idiom for "this
  spec is bound by that one" (spec 045 uses it against 013 already), and it
  keeps 013 surfaced as an authority over this work. Spec 013 keeps FR-008,
  which forbids claiming the erasure reaches these records until FR-007
  shows it.

  Rejected: a placeholder `erasure.rs` in 013's session, which would have
  put this spec's file in another spec's change; implementing this spec
  early to make the unit exist, which is a second spec in one session; a
  `Spec-Drift-Waiver:`, which records a contradiction rather than resolving
  it; and amending the contract's lifecycle table to exempt `constrains`
  generally, which the maintainer explicitly declined to authorize.
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
