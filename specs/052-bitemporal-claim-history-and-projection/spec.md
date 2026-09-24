---
id: "052-bitemporal-claim-history-and-projection"
title: "Bitemporal claim history: valid time apart from record time, supersession per slot, and a current view that is a pure function"
status: draft
kind: "kernel"
domain: "memory"
created: "2026-09-24"
authors: ["Bartek Kus"]
implementation: pending
risk: critical
wave: 5
depends_on:
  - "051-claim-admission-and-authority"
establishes:
  - "crates/aicortex-claims/src/history.rs"
  - "crates/aicortex-claims/src/asof.rs"
  - "crates/aicortex-claims/src/policy.rs"
  - "crates/aicortex-claims/src/projection.rs"
  - "crates/aicortex-claims/tests/projection.rs"
  - "crates/aicortex-claims/tests/reorder.rs"
  - "crates/aicortex-claims/testdata/travel/"
  - "crates/aicortex-store/src/claim_repo.rs"
  - "crates/aicortex-store/tests/claim_history.rs"
  - "crates/aicortex-store/tests/claim_erasure.rs"
extends:
  - { spec: "050-typed-claims-and-predicate-registry", unit: "crates/aicortex-claims/src/lib.rs", nature: additive }
  - { spec: "050-typed-claims-and-predicate-registry", unit: "crates/aicortex-claims/Cargo.toml", nature: additive }
  - { spec: "012-store-schema-and-repositories", unit: "crates/aicortex-store/src/lib.rs", nature: additive }
  - { spec: "012-store-schema-and-repositories", unit: "crates/aicortex-store/src/migrations.rs", nature: additive }
  - { spec: "014-memory-lifecycle-and-erasure", unit: "crates/aicortex-store/src/erasure.rs", nature: additive }
constrains:
  - { flavor: invariant-freeze, unit: "crates/aicortex-store/src/claim_repo.rs", note: "claim history is append-only: no statement updates or deletes a claim or relation row except erasure's tombstone" }
  - { flavor: invariant-freeze, unit: "crates/aicortex-claims/src/projection.rs", note: "the current view is a pure function of claim history, an explicit as-of on both axes, and a policy version" }
references:
  - { unit: { kind: file, path: "specs/014-memory-lifecycle-and-erasure/spec.md" }, role: constraint }
  - { unit: { kind: file, path: "specs/017-entities-and-typed-edges/spec.md" }, role: context }
  - { unit: { kind: file, path: "specs/018-retrieval-and-recall-trace/spec.md" }, role: context }
summary: >
  A claim has three times: when it holds in the world (valid time, with
  precision, zone, open ends, and uncertainty), when the source said so
  (source-declared time), and when this store learned it (transaction
  time). History is append-only on the transaction axis; retraction,
  correction, and supersession are appended records, and only erasure
  removes content, leaving a tombstone. Supersession is scoped to one
  subject, predicate, and slot, so a schedule change to one segment
  supersedes that attribute alone. The current view is a deterministic
  projection of history under an explicit as-of on both axes and a policy
  version, ordered by the source's own time and the claim's authority,
  never by arrival, so reordered email delivery yields the same answer.
---

# 052: Bitemporal claim history and projection

## 1. Purpose

Travel email arrives out of order. A schedule change sent on Wednesday can
be delivered before the booking confirmation sent on Monday; a forwarded
copy of an old itinerary arrives a week later. A store that takes the
latest-arriving message as the truth will, on exactly those days, tell a
traveler the wrong departure time. The same store, asked "what did we
believe on Tuesday", cannot answer, because it overwrote Tuesday's belief.

This spec separates the question "when is this true" from "when did we
know it", keeps every past belief, and computes the present from the past
by a function that does not depend on the order in which facts arrived.
Spec 014 gave a memory supersession, correction, and expiry as whole-record
status changes; that is right for prose memories and too coarse for a
segment whose seat changed while its departure time did not. Claims get
their own lifecycle here; 014's lifecycle for memories is unchanged.

## 2. Territory

The history, as-of, policy, and projection modules of `aicortex-claims`
(pure), the claim repository of `aicortex-store` with its migration, and an
additive extension of 014's erasure so erasure reaches claims.

## 3. Behavior

### 3.1 Three times

- **B-1 (the claim record).** `ClaimRecord { claim: Claim (050), valid:
  ValidTime, source_time: Option<TimePoint>, source_seq:
  Option<SourceSeq>, tx: TxStamp, authority: AuthorityLevel (051),
  admission: AdmissionRef }`. `ValidTime` is an `Interval` of 050's time
  points, half-open, either end possibly open, each bound carrying its
  precision, zone, and uncertainty. `source_time` is the time the source
  declares for its own statement (an email's `Date`, an itinerary's
  "issued"), which is distinct from valid time and from both 011
  provenance times. `source_seq` is a source-supplied revision where one
  exists (a booking revision number, a ticket reissue counter).
- **B-2 (valid-time modes).** The predicate's registry entry (050 B-8)
  declares one of: `Explicit` (the claim states its own validity),
  `FromSourceTime` (valid from `source_time`, open-ended until superseded:
  "the departure is now 10:05" as of the change notice), or `Timeless` (an
  identifier such as a record locator). A claim whose predicate is
  `Explicit` and that carries no valid time is refused at admission.
  A value that is itself an interval (a hotel stay) is a value, not valid
  time: the stay's dates are what the claim says, and its valid time is
  when that booking state held.
- **B-3 (transaction time).** `TxStamp { seq: u64, recorded_at:
  UnixSeconds }`. `seq` is assigned inside the writing transaction as one
  more than the scope's highest, so it is gap-free and strictly increasing
  per scope under hiqlite's serialized writes, and `recorded_at` is the
  commit's wall clock. Transaction time is never taken from the source and
  never edited.

### 3.2 Append-only history

- **B-4 (append, never rewrite).** A claim row, a relation row (050 B-11),
  and a retraction row are inserted and never updated or deleted, except
  by erasure (B-10). There is no `status` column on a claim that a later
  event rewrites. Whether a claim is current is always computed (B-7).
- **B-5 (supersession is scoped).** Supersession relates claims in one
  slot: the same scope, subject, predicate, and slot key (050 B-4). An
  explicit `Supersedes` relation across slots is refused at append. Within
  a slot, the predicate's registered rule (050 B-8) is one of:
  `BySourceOrder` (a claim with a later `(source_seq, source_time)`
  supersedes an earlier one in the overlap of their valid times),
  `ExplicitOnly` (only a `Supersedes` relation supersedes), or `Accumulate`
  (values coexist; cardinality `many` without replacement). A supersession
  affects the superseded claim over the overlap only: a change effective
  from Wednesday leaves Monday's value current for Monday.
- **B-6 (retraction and correction).** A retraction is an appended
  `Retraction { target: ClaimId, reason, by: Actor, provenance }`, admitted
  through 051 like any claim-level act. A correction is a new claim plus a
  `Supersedes` relation at `UserCorrected` authority (051 B-8). Neither
  removes or edits the target row. A retracted claim stays in history and
  is excluded from views whose knowledge bound includes the retraction.

### 3.3 Projection

- **B-7 (projection is a pure function).** `project(&ClaimHistory,
  AsOf, &ProjectionPolicy) -> CurrentView` reads nothing but its
  arguments: no clock, no randomness, no store, no host time zone. `AsOf {
  knowledge: TxBound, valid: ValidBound }` is required on both axes and
  has no default; `TxBound` is a sequence number or a `recorded_at`
  instant, and `ValidBound` is an instant or an interval. A caller that
  wants "now" passes its own clock's reading, and the view echoes the
  as-of it was computed under and the history's high-water `seq`.
- **B-8 (winner selection).** For each slot, among claims with `tx.seq`
  within the knowledge bound, not retracted within it, and whose valid time
  covers the valid bound, the projection applies, in order: explicit
  `Supersedes` relations at sufficient authority (051 B-10); then higher
  `AuthorityLevel`; then the predicate's rule (B-5) by `source_seq`, then
  `source_time`. It never orders by `tx.seq`, `recorded_at`, or `ClaimId`,
  all of which encode arrival. When two candidates remain tied and their
  values are equal, they are one value with corroborating claims. When
  they remain tied with different values, the slot's state is
  `Conflicted` with both candidates, and a `Contradicts` relation is
  proposed for review (017 B-10's rule: surfaced, not resolved).
- **B-9 (comparison under precision and uncertainty).** A bound at day
  precision covers its whole civil day in its zone; a bound with
  uncertainty covers its window; a `Floating` time widens by the policy's
  floating window (default fourteen hours either side). A claim whose
  valid time certainly covers the valid bound is `Definite`; one that
  covers it only within widened bounds is `Possible`. The view reports the
  certainty per slot. Zone conversion uses the time-zone database version
  the policy pins.
- **B-10 (view contents).** `CurrentView` lists, per slot, the state
  (`Value`, `Conflicted`, `Retracted`, `Absent`), the winning claim ids,
  the certainty, the claims each winner superseded in the window, and the
  policy version and as-of used, in a canonical order, so that two views
  are comparable byte for byte and a view has a reproducible digest.
- **B-11 (policy version).** `ProjectionPolicy` is a versioned document
  with a digest: the floating window, the time-zone database version, and
  whether `Possible` coverage counts toward a winner. A view names the
  policy version; the same history, as-of, and policy version always yield
  the same view digest.

### 3.4 Erasure and the store

- **B-12 (erasure reaches claims).** Erasing a claim (an extension of
  014 B-7) removes its value, its spans and their digests, its
  qualifiers, its subject key, and its slot key, and leaves a tombstone row
  with its id, scope, subject namespace and kind, predicate, times, and
  authority, so relations resolve to a tombstone rather than dangle. The
  subject and slot keys are blanked because a domain-minted key (a record
  locator, a traveler key) can itself be personal data (D-6); a tombstone
  is therefore no longer attributable to its subject, and a view over that
  subject no longer sees it. Erasing a source observation marks every claim citing it
  `origin_erased`, as 014 B-8 does for derived memories, and cascades to
  those claims only when the erasure call asks. Erasing a scope erases
  every claim, relation, retraction, proposal, and admission record in it.
  The ledger Decision names counts, never values.
- **B-13 (transactional append).** An admitted claim, its relations, its
  admission record (051 B-11), its counter updates, and its outbox work are
  staged into one rahi transaction (constitution XI, 012 B-4). The
  repositories stage into the caller's `TxnBuilder` and never open their
  own, so a consumer cell that links aicortex as a library (050 D-4) also
  commits its inbound receipt (`rahi://045`, draft) and the source
  observation in the same transaction. Every
  repository statement carries the scope predicate (012 B-3).
- **B-14 (history reads are bounded).** `ClaimRepo::history(scope,
  subject, predicate?, TxBound)` pages by `(tx.seq)` and is bounded by the
  subject; there is no unscoped or unbounded read. A materialized view
  cache, if a later spec adds one, is keyed by policy version and
  high-water `seq` and must equal the projection it caches.

## 4. Functional requirements

- **FR-001.** The travel fixture: a trip with two segments; email A (sent
  Monday) books both; email B (sent Wednesday) changes segment 2's
  departure; email C (sent Tuesday) assigns a seat on segment 1. For all
  six delivery orders, the view at full knowledge and valid time Thursday
  shows segment 2's departure from B, segment 1's departure from A, and
  segment 1's seat from C, with an identical view digest.
- **FR-002.** Over the same fixture delivered in the order B, C, A, the
  view at the knowledge bound after B alone shows B's departure for
  segment 2 and nothing for segment 1, and the view at valid time Monday with full knowledge shows
  A's original departure for segment 2.
- **FR-003.** A property test over generated slot histories asserts that
  permuting delivery order never changes the full-knowledge view digest.
- **FR-004.** A superseded, a corrected, and a retracted claim each
  remain readable by id with their original bytes, and a direct count of
  claim rows never decreases across any operation other than erasure.
- **FR-005.** Two same-authority claims with equal source order and
  different values produce `Conflicted` and one `Contradicts` proposal;
  neither is chosen.
- **FR-006.** A `Supersedes` relation across two slots is refused.
- **FR-007.** A `Floating` departure time and a day-precision stay bound
  project as `Possible` or `Definite` exactly as the fixture records.
- **FR-008.** `project` compiles in a crate with no I/O, clock, or random
  dependency, and a test runs it twice over the same inputs on two threads
  and compares digests.
- **FR-009.** After erasing a source observation with cascade, no claim
  value, span, subject key, or slot key from it is readable, its claims
  are tombstones, and a view over the slot reports `Absent` or the next surviving claim; without
  cascade, its claims are marked `origin_erased` and still project.

## 5. Acceptance criteria

- **AC-1.** `cargo test -p aicortex-claims --locked` passes, including
  `projection` and `reorder`.
- **AC-2.** `cargo test -p aicortex-store --locked --test claim_history`
  and `--test claim_erasure` pass against rahi's single-voter harness.
- **AC-3.** 014's own erasure tests still pass with this extension
  present.

## 6. Out of scope

Recall over claims and how a claim ranks against memories (018). An HTTP or
MCP surface for as-of queries (a later extension of 020 and 021). The
review queue that resolves `Conflicted` slots (023). Semantic
near-duplicate detection across claims (034). Expiry as a background pass:
a claim's end of validity is part of its valid time and is honored by
projection, so no curator rewrites a claim when it stops holding.

fact-fold's transaction-time datom log and retraction folding are prior art
for B-3 to B-6 and are not imported (050 section 6.1). This spec adds the
valid-time axis fact-fold does not have. statecraft-review's `fold.rs`
applies the same rule as B-7, a view folded from the log and nothing else,
to a different log; the two are recorded as overlapping and not merged.

## 7. Resolved decisions

- **D-1 (2026-09-24, owner).** Claims are bitemporal: valid time is
  distinct from transaction time and from source-declared time, and as-of
  queries run on both axes.
- **D-2 (2026-09-24, owner).** Retraction and correction are appended and
  never erase. Only erasure removes content (constitution XIII).
- **D-3 (2026-09-24, owner).** The current view is a deterministic
  projection with an explicit as-of clock, and reordered delivery must not
  change it: no latest-email-wins.
- **D-4 (2026-09-24, owner).** Supersession is scoped to a subject and
  predicate: a later observation supersedes one attribute of a segment
  without superseding its siblings.
- **D-5 (2026-09-24, owner decision).** Recorded from the owner's answer
  to this draft's questions: "I agree with all your suggestions and
  recommendations." Claims are written by a consumer that links aicortex
  as a library inside its own rahi cell (050 D-4), so B-13's transaction
  includes the consumer's receipt and observation.
- **D-6 (2026-09-24, owner decision).** Erasure blanks the subject key and
  the slot key on a claim tombstone (B-12). This resolves Q-3.

## 8. Open questions

Q-3 was resolved by D-6.

- **Q-1 (source order without a source time).** When neither
  `source_seq` nor `source_time` is present, B-8 falls through to
  authority and then to `Conflicted`. Should a policy instead allow
  transaction order as a last resort for specific predicates, knowing it
  reintroduces arrival dependence?
- **Q-2 (materialized views).** Compute the view on read, or maintain a
  cache keyed by policy version and high-water `seq`? The draft permits a
  cache only if it equals the projection (B-14) and does not specify one.
- **Q-4 (019 framing).** A projected value returned to a model is recalled
  content under constitution X. Does the view carry 019's delimiting at
  this layer, or only at the surfaces?
- **Q-5 (uncertainty defaults).** Is fourteen hours the right floating
  window, and should the time-zone database version move only with a
  policy version (as drafted) or also with the toolchain?
- **Q-6 (memory valid_until).** 014 B-5 gives a memory a `valid_until`
  that 011's record does not carry. Should memories adopt 050's time types
  for it, or is that gap 014's to close on its own?

## 9. Obligations

Declared in the spec-spine 106 grammar, to be lifted into the frontmatter
`obligations` key when this repository's spec-spine pin moves to 0.25.0,
which is a separate follow-up (050 D-8; below 0.25.0 the key is a compile
error, `V-002`).

```yaml
obligations:
  - { id: "I-1", kind: invariant, text: "Correction, retraction and supersession never delete or update prior claim rows; only erasure tombstones one.", anchor: "3-2-append-only-history" }
  - { id: "I-2", kind: invariant, text: "The projection is a pure function of claim history, an explicit as-of on both axes, and a policy version.", anchor: "3-3-projection" }
  - { id: "I-3", kind: invariant, text: "Winner selection never orders by transaction time, recorded_at or claim id, so delivery order cannot change the full-knowledge view.", anchor: "3-3-projection" }
  - { id: "I-4", kind: invariant, text: "Supersession never crosses a (scope, subject, predicate, slot).", anchor: "3-2-append-only-history" }
  - { id: "I-5", kind: invariant, text: "Transaction time is assigned by the store, strictly increasing per scope, and never edited.", anchor: "3-1-three-times" }
  - { id: "R-1", kind: requirement, text: "Two tied candidates with different values are reported as Conflicted and never silently resolved.", anchor: "3-3-projection" }
  - { id: "R-2", kind: requirement, text: "Erasure reaches claim values, spans, subject keys and slot keys, and leaves a tombstone that relations resolve to.", anchor: "3-4-erasure-and-the-store" }
```

## Verification

```verify:cli
# planned: the modules, fixtures and tests below exist once this spec is built.
cargo test -p aicortex-claims --locked --test projection
cargo test -p aicortex-claims --locked --test reorder
cargo test -p aicortex-store --locked --test claim_history
cargo test -p aicortex-store --locked --test claim_erasure
cargo test -p aicortex-store --locked --test erasure
```
