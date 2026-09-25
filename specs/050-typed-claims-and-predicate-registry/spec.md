---
id: "050-typed-claims-and-predicate-registry"
title: "Typed claims: a subject, a registered predicate, a typed value, an epistemic status, and the source span it came from"
status: approved
kind: "kernel"
domain: "memory"
created: "2026-09-24"
authors: ["Bartek Kus"]
implementation: pending
risk: critical
wave: 5
depends_on:
  - "014-memory-lifecycle-and-erasure"
  - "046-rahi-0-2-0-adoption"
establishes:
  - "crates/aicortex-types/src/claim.rs"
  - "crates/aicortex-types/src/claim_value.rs"
  - "crates/aicortex-types/src/claim_time.rs"
  - "crates/aicortex-types/src/predicate.rs"
  - "crates/aicortex-types/src/span.rs"
  - "crates/aicortex-types/tests/claim.rs"
  - "crates/aicortex-types/testdata/claims/"
  - "crates/aicortex-claims/Cargo.toml"
  - "crates/aicortex-claims/src/lib.rs"
  - "crates/aicortex-claims/src/registry.rs"
  - "crates/aicortex-claims/tests/registry.rs"
  - "crates/aicortex-claims/testdata/registries/"
  - "crates/aicortex-store/src/predicate_registry_repo.rs"
  - "crates/aicortex-store/tests/predicate_registry.rs"
extends:
  - { spec: "010-chassis-adoption-and-workspace", unit: { kind: section, file: "Cargo.toml", anchor: "workspace" }, nature: additive }
  - { spec: "010-chassis-adoption-and-workspace", unit: { kind: section, file: "Cargo.toml", anchor: "workspace.dependencies" }, nature: additive }
  - { spec: "011-memory-model", unit: "crates/aicortex-types/src/lib.rs", nature: additive }
  - { spec: "011-memory-model", unit: "crates/aicortex-types/src/provenance.rs", nature: additive }
  - { spec: "012-store-schema-and-repositories", unit: "crates/aicortex-store/src/lib.rs", nature: additive }
  - { spec: "012-store-schema-and-repositories", unit: "crates/aicortex-store/src/migrations.rs", nature: additive }
  - { spec: "001-agentic-harness", unit: "standards/spec/contract.md", nature: additive }
amends:
  - "002-memory-thesis"
amends_sections:
  - "2-the-nine-responsibilities"
  - "4-crate-topology"
  - "5-build-order"
constrains:
  - { flavor: invariant-freeze, unit: "crates/aicortex-types/src/claim.rs", note: "a claim cannot be constructed without a subject, a registered predicate reference, a typed value, an epistemic status and provenance" }
references:
  - { unit: { kind: file, path: "specs/011-memory-model/spec.md" }, role: constraint }
  - { unit: { kind: file, path: "specs/017-entities-and-typed-edges/spec.md" }, role: context }
  - { unit: { kind: file, path: "docs/design/00-lineage.md" }, role: context }
summary: >
  aicortex becomes the family's canonical owner of claim history with
  provenance. This spec defines what one claim says, as types, before any
  claim is admitted or stored: a subject reference, a predicate from a
  namespaced, versioned registry that a domain supplies, a typed value
  (text, integer, fixed-point decimal with a unit or currency, date,
  datetime with zone, interval, entity reference, enumeration), an epistemic
  status taken from the source's own stance, typed claim-to-claim relations,
  and source span references that extend 011's Provenance with a part, a
  byte or character span, and a content digest. aicortex stays domain
  agnostic: travel, the first consumer, registers its own predicates. Who may
  admit a claim is 051; when it holds and how history is kept is 052.
---

# 050: Typed claims and the predicate registry

## 1. Purpose

A memory (011) is a claim in prose: someone said this text, from this
source, at this time. That is enough for recall and not enough for a system
that must answer "what is the departure time of segment 2 of this trip, as
we know it now, and which email says so". The first consumer, travel-memory,
is a Rust domain service on the rahi chassis that ingests travel email into
typed temporal claims. It needs structured values, a stable vocabulary it
controls, and a pointer from every value back to the exact bytes that
support it.

The owner has decided that this history is aicortex's to own for the whole
family, rather than each product keeping its own fact store, and that
fact-fold is demoted (section 7, D-1 and D-2). A family-wide owner must stay
domain agnostic, so the vocabulary is supplied by domains through a
registry rather than compiled into this repository.

The shape follows 011's rule: the shape is types, the invariants are
unrepresentable if violated, and a model that proposes a claim proposes
values into a fixed structure. The predecessor's free-form metadata bag
(`openbrain://json-bag-schema`) is the failure this avoids a second time.

## 2. Territory

- `aicortex-types` gains five modules: `claim.rs` (the claim content and
  its identifiers, relations, and epistemic status), `claim_value.rs` (the
  typed values), `claim_time.rs` (time points, precision, zones, intervals,
  and uncertainty, shared by values and by 052's valid time), `predicate.rs`
  (the registry document and predicate references), and `span.rs` (source
  span references). `provenance.rs` is extended additively with spans.
- A new pure crate, `aicortex-claims`, with no I/O, no SQL, and no async,
  holding the validation of a claim against a registry snapshot. It is the
  seam an embedding consumer can link without a store; 052 adds history and
  projection to it.
- `aicortex-store` gains the registry repository and its migration.

The types are the contract a consumer writes against. Admission (051) and
history (052) build on them and do not change them.

The consumer links these crates as libraries inside its own rahi cell
(D-4). Every repository in 050 to 052 therefore stages into a caller's
`TxnBuilder` (012 B-4) and never opens a transaction of its own, so a
consumer can commit its inbound receipt (`rahi://045`, draft), the source
observation, the admitted claims, and their outbox work in one
transaction.

Operator prerequisite: library mode needs rahi's named migration sets
(`rahi://named-migration-sets`, planned, being drafted in the rahi
repository), so aicortex's migrations are registered as their own set in
the host cell and never renumbered into its sequence (D-9). Until a rahi
release carries it, library mode is blocked.

## 3. Behavior

### 3.1 The claim

- **B-1 (claim content).** `Claim { id: ClaimId, scope: Scope, subject:
  SubjectRef, predicate: PredicateRef, value: ClaimValue, slot:
  Option<SlotKey>, epistemic: EpistemicStatus, provenance: Provenance,
  schema_version: u16 }`. `ClaimId` wraps a UUIDv7 minted by the writer, as
  `MemoryId` does (011 B-1). Valid time and source-declared time are
  attached by 052 around this content, not inside it, so that the content
  of a claim is what was said and the history is when.
- **B-2 (no claim without provenance).** `Claim::new` takes `Provenance`
  by value. There is no `Default`, no builder that can finish without it,
  and no deserialization path that fills it in. A claim derived from a
  source names the source memory in `derived_from` and at least one
  `SourceSpan` (B-12) when the source is text the store holds. This is
  constitution IX applied to claims.
- **B-3 (subject).** `SubjectRef { namespace: Namespace, kind: SubjectKind,
  key: SubjectKey }`, for example `travel` / `segment` / an opaque key the
  domain mints. Subject kinds are declared by the registry (B-8). A subject
  is scoped: the same key in two scopes is two subjects. A subject may be
  linked to an entity of 017; that link is not required and 017's closed
  `EntityKind` is not widened by it (open question Q-3).
- **B-4 (slot).** A predicate with cardinality `many` (B-9) distinguishes
  its values by a `SlotKey` the registry declares (for example a passenger
  key for `travel:segment.seat`). Supersession (052) is scoped to one
  `(subject, predicate, slot)`, never to the subject as a whole.

### 3.2 Typed values

- **B-5 (value types).** `ClaimValue` is a closed enum:
  - `Text(String)`, normalized as the gate normalizes (013 B-8);
  - `Integer(i64)`;
  - `Decimal { mantissa: i128, scale: u8, unit: Option<Unit> }`, fixed
    point, where `Unit` is an ISO 4217 currency code or a registered
    measurement unit. There is no floating-point value, because a value
    must compare and hash identically on every target (the reasoning of
    013 D-4);
  - `Date(CivilDate)` with a `Precision` of year, month, or day;
  - `DateTime(ZonedTime)`, a civil date and time with a `Precision` down to
    the second and a `Zone` (B-6);
  - `Interval(Interval)`, two bounds of `Date` or `DateTime`, either of
    which may be open (B-7);
  - `EntityRef(SubjectRef)`, a reference to another subject;
  - `Enum(EnumVariant)`, a variant of an enumeration the predicate
    declares.
  An unknown discriminant is a typed deserialization error naming the
  field (011 FR-002), never a passthrough.
- **B-6 (zones and offsets).** `Zone` is `Iana(TzName)`, `Offset(i32
  seconds)`, or `Floating` (the source gave a wall-clock time with no zone,
  as a boarding pass often does). A value keeps the zone form the source
  gave. Conversion to an instant is a function of the value and a pinned
  time-zone database version, never of the host's zone; the database
  version is part of 052's projection policy.
- **B-7 (intervals, precision, uncertainty).** A bound is `Open`,
  `Inclusive(TimePoint)`, or `Exclusive(TimePoint)`. A `TimePoint` carries
  its precision, so "2026-10-03" at day precision is not silently read as
  midnight. A time point may carry `Uncertainty`, an explicit earliest and
  latest instant, when the source states a window ("arrives between 14:00
  and 16:00"). Precision and uncertainty are preserved as given; widening a
  day into an instant range happens only at comparison time (052).

### 3.3 Epistemic status and relations

- **B-8 (the registry document).** A domain supplies a `PredicateSet {
  namespace, version, subject_kinds, predicates }`. Each `PredicateDef`
  names the predicate, the subject kinds it applies to, its value type
  (and, for `Enum`, its variants, and for `Decimal`, the admitted units),
  its cardinality, its slot key when cardinality is `many`, its valid-time
  mode (052 B-2), its supersession rule (052 B-5), and the epistemic
  statuses it admits.
- **B-9 (namespaces and versions).** A namespace is lowercase ascii with
  `-` and `_`. The namespace `aicortex` is reserved. A `PredicateRef` is
  `namespace:name@version`. A registered version is immutable. A later
  version is a new document that may add predicates, variants, and units,
  and may deprecate a predicate; it may not change the value type or
  cardinality of an existing predicate, because claims written under the
  earlier version must keep their meaning. A claim records the version it
  was written under.
- **B-10 (epistemic status).** `EpistemicStatus` is a closed enum recording
  the source's stance toward the statement, not aicortex's belief in it:
  `Asserted` (stated as so), `Denied` (stated as not so), `Conditional`
  (holds if a stated condition holds, with the condition as a `Text`
  qualifier), `Requested` (asked for, for example a seat request),
  `Confirmed` (acknowledged by the party able to make it so, for example
  a carrier confirmation), `Estimated` (an approximate value, for example
  an estimated arrival), `Expected` (scheduled or planned), and `Observed`
  (recorded as having happened, for example a boarding scan). Authority,
  which is aicortex's own weighing, is a separate axis (051).
- **B-11 (relations).** `ClaimRelation { from: ClaimId, kind:
  RelationKind, to: RelationTarget, provenance }` where `RelationKind` is
  `Supersedes`, `Contradicts`, or `DerivedFrom`, and a target is a claim
  (all three) or a memory (`DerivedFrom` only). A relation is its own
  record with its own provenance, appended beside the claim, so that a
  later observation can relate to an earlier claim without editing it. The
  meaning of `Supersedes` over time is 052's.

### 3.4 Source spans

- **B-12 (span references).** `SourceSpan { source: MemoryId, part:
  PartLocator, range: SpanRange, digest: ContentDigest }`. `PartLocator`
  names the part of a multi-part source (a MIME part path, an attachment
  name, or `body`). `SpanRange` is `Bytes { start, end }` over the part's
  stored bytes or `Chars { start, end }` over its Unicode scalar values,
  half-open, and says which. `ContentDigest` is the algorithm identifier,
  a key id, and a keyed digest of the whole part (HMAC-SHA-256 under a key
  held per scope in the application store, as 013 B-9 holds its keys), so
  a reader holding the scope can tell whether the bytes a span points into
  are still the bytes it was measured against, and a digest of a short part
  cannot be confirmed by guessing. Erasing a scope destroys its span key
  (D-6).
- **B-13 (Provenance gains spans).** `Provenance` gains `spans:
  Vec<SourceSpan>`, serialized only when non-empty, so every existing 011
  fixture round-trips byte-identically (011 FR-001). A span carries no
  content: offsets and a digest only.
- **B-14 (validation is pure).** `aicortex_claims::validate(&Claim,
  &RegistrySnapshot) -> Result<ValidClaim, ClaimError>` checks the
  predicate is registered at the named version and not withdrawn, the
  subject kind is one the predicate applies to, the value matches the
  declared type, unit, and variant set, the slot is present exactly when
  cardinality is `many`, and the epistemic status is admitted. It has no
  clock, no randomness, and no I/O. An unknown predicate is refused and
  counted, never stored as free text (017 B-5's rule, applied here).
- **B-15 (registration is recorded).** Registering a `PredicateSet`
  version writes it to the `predicate_registry` table in one transaction
  and appends a ledger Decision naming the namespace, the version, the
  document digest, and the registering subject. The document is
  vocabulary, not memory content, so it may be named in the chain.

## 4. Functional requirements

- **FR-001.** Every fixture in `testdata/claims/` round-trips
  byte-identically, and every 011 memory fixture still round-trips after
  `spans` is added.
- **FR-002.** A deserialization of an unknown value type, zone form,
  precision, epistemic status, or relation kind fails with a typed error
  naming the field.
- **FR-003.** A compile-fail test asserts that `Claim` has no constructor
  reachable without a `Provenance`.
- **FR-004.** Over `testdata/registries/`, `validate` accepts each valid
  claim and refuses each invalid one with the recorded error: unknown
  predicate, wrong version, wrong subject kind, wrong value type, undeclared
  unit, undeclared enum variant, missing slot, and inadmissible epistemic
  status.
- **FR-005.** Registering a version that changes the value type or
  cardinality of an existing predicate is refused; registering the same
  version twice with different content is refused; registering it twice
  with identical content is a no-op.
- **FR-006.** A decimal value compares equal to itself across
  serialization and never passes through a floating-point type, asserted by
  a test over the crate's public API.
- **FR-007.** A travel registry fixture declares at least `segment`,
  `booking`, and `traveler` subject kinds and predicates covering a
  departure time (`DateTime`), a seat (`Text`, cardinality `many` keyed by
  traveler), a fare (`Decimal` with currency), a stay interval
  (`Interval` of `Date`), a booking status (`Enum`), and a segment's
  booking (`EntityRef`), and every one validates.

## 5. Acceptance criteria

- **AC-1.** `cargo test -p aicortex-types --locked` passes, including the
  compile-fail case and every pre-existing 011 fixture.
- **AC-2.** `cargo test -p aicortex-claims --locked --test registry`
  passes.
- **AC-3.** `cargo test -p aicortex-store --locked --test
  predicate_registry` passes against rahi's single-voter harness.
- **AC-4.** `aicortex-claims` has no dependency on any I/O, SQL, or async
  crate, asserted as 011 AC-2 asserts it for `aicortex-types`.

## 6. Out of scope

Admission and authority (051). Valid time, transaction time, as-of
queries, supersession semantics, and projection (052). An HTTP or MCP
surface for claims, which is a later extension of 020 and 021. Extraction
of claims from email, which is travel-memory's, not aicortex's. Any travel
vocabulary beyond the test fixture: the travel registry is authored and
versioned in travel-memory.

### 6.1 Relationship to other models in the family

- **fact-fold (demoted).** fact-fold is a TypeScript fact store over
  libSQL: dictionary-encoded datoms `(entity, attribute, value, time, op)`,
  time travel by folding retractions over transaction time, SKOS-style
  closure by recursive CTEs, and graph-scoped policy by predicate
  injection. It is not a dependency of this corpus and not a peer store.
  Its transaction-time datom model and its retraction folding are prior art
  for 052's transaction axis, and its scoped graph closure may inform spec
  017's bounded walk. Neither is imported; both are cited as design input.
- **statecraft-envelope `fact.rs`.** A content-addressed fact envelope
  `{ kind, v, body, extra }` with tombstones, used by the Statecraft
  platform's scope log. It overlaps with this spec's claim record and with
  052's append-only history. Recorded as an overlapping model; not merged
  or aligned here (Q-5).
- **statecraft-review `fold.rs`.** Folds a revision's account from its
  scope's entries and nothing else. It is the same discipline as 052's
  projection (a view is a pure function of the log) over a different log.
  Recorded as an overlapping model; not merged here.

## 7. Resolved decisions

- **D-1 (2026-09-24, owner).** aicortex is the family's canonical owner of
  claim history with provenance. A product that needs typed temporal claims
  stores them here rather than in its own fact store. This amends spec 002
  (a new responsibility, a new crate in the topology, and the placement of
  these specs in the build order); the amendment is carried by this spec's
  `amends` edge and takes effect only when a human approves this spec.
- **D-2 (2026-09-24, owner).** fact-fold is demoted. It is not the family's
  claim store and aicortex does not depend on it.
- **D-3 (2026-09-24, owner).** The first consumer is travel-memory, a Rust
  domain service on the rahi chassis that ingests travel email into typed
  temporal claims. aicortex stays domain agnostic: the predicate registry
  is supplied by domains, and travel defines its own predicates.

- **D-4 (2026-09-24, owner decision).** Recorded from the owner's answer
  to this draft's questions: "I agree with all your suggestions and
  recommendations." travel-memory links aicortex as a library inside its
  own rahi cell, not over HTTP, so that its inbound receipts (rahi spec
  045, `rahi://045`), claim writes, and outbox work commit in one
  transaction. Constitution VIII holds per cell: the claims live in the
  consumer's own hiqlite group through rahi's store API. A claims surface
  in 020 is not required for the first consumer.
- **D-5 (2026-09-24, owner decision, placement).** The owner asked for the
  option that best fits 002 D-4 and the wave rules. The three claim specs
  open a new wave 5 at ordinals 050 to 052; the two housekeeping specs stay
  in wave 4 at 046 and 047. Reasoning: wave 2 (020 to 029) is defined by
  needing a rauthy binary on loopback and holds the surfaces, which claims
  need neither, and 002 D-4 keeps 025 unallocated for an earlier
  coordination core, a different reservation that this should not consume.
  Wave 4 is proof (evaluation, privacy, portability, packaging,
  deployment) with 045 as its one feature, and claims are neither, so
  leaving them at 047 to 049 would have placed them by free ordinal rather
  than by meaning. A new wave keeps both existing reservations intact and
  makes the family adoption visible in the build order. Every dependency
  still points into the same or an earlier wave (014 is wave 1; 046 is
  wave 4), and the scheduler's lowest-numbered-ready rule lets 050 start as
  soon as 014 and 046 are complete. The housekeeping specs stay in wave 4
  because they maintain wave 1 territory and 046 must precede 050. Section
  10 states the amendment of 002 this implies.
- **D-6 (2026-09-24, owner decision).** Span digests are keyed per scope
  (B-12), and erasing a scope destroys the key, for the reason 013 D-2
  gives for Decision digests. Erasing a claim removes its span digests with
  its value (052 B-12).
- **D-7 (2026-09-24, owner decision).** Converging `SubjectRef` with 017's
  entities is deferred and stays open (Q-3, Q-4). Subjects are separate
  from entities, with an optional link, until the owner decides.
- **D-8 (2026-09-24, owner decision).** The family moves to spec-spine
  0.25.0, and for this repository that is a separate step: the pin lives
  in spec 001's territory (`spec-spine.toml`, `govern.yml`, `AGENTS.md`,
  `README.md`) and moves by the same kind of change as 001 D-7. Until it
  moves, obligations stay in section 9 of each spec rather than in
  frontmatter.
- **D-9 (2026-09-24, owner decision).** The owner's answer: "I agree with your recommendations so proceed". A linked
  library registers its own named migration set in the host cell, and the
  host tracks each set's version independently; aicortex never renumbers
  its migrations into the host's sequence. This needs chassis support,
  which is being drafted separately in the rahi repository as "named
  migration sets". Recorded here as a planned external reference,
  `rahi://named-migration-sets`, with no rahi spec id yet: that spec is not
  in rahi's corpus, so no edge can name it. aicortex's library mode (D-4)
  is blocked on it. A build session for 050 to 052 that finds no released
  rahi with named migration sets stops and reports the missing
  prerequisite rather than composing migrations by hand (AGENTS.md,
  "Working the backlog" step 1). This resolves Q-8.

## 8. Open questions

Q-1 (consumption mode), Q-6 (span digests), Q-7 (build order), and Q-8
(composing migrations) were resolved by D-4, D-6, D-5, and D-9 and are not
repeated here.

- **Q-2 (who registers).** Which subject may register a namespace and a
  version: an operator only, the namespace's first registrant, or a
  subject granted per namespace? And does a domain register at its own
  `migrate`, or through an operator call?
- **Q-3 (subjects and 017 entities, deferred by D-7).** Should
  `SubjectRef` and 017's `Entity` converge (a subject is an entity of an
  open, registered kind), or stay separate with an optional link? 017 is approved and pending, so
  converging would be an amendment of 017 before it is built.
- **Q-4 (017's closed vocabulary).** 017 D-1 keeps the edge predicate
  vocabulary closed and makes a new predicate an amendment. This spec's
  registry is open to domains but closed per registered version. Should
  017's edges become a projection of claims under a reserved `aicortex`
  namespace, or stay a separate, closed vocabulary?
- **Q-5 (statecraft-envelope alignment).** Should a claim have a canonical
  content-addressed encoding compatible with statecraft-envelope's
  `FactEnvelope`, so a claim can be cited from a Statecraft scope by
  digest? Not decided, and not required by travel-memory.
## 9. Obligations

Declared in the spec-spine 106 grammar, to be lifted into the frontmatter
`obligations` key when this repository's spec-spine pin moves to 0.25.0,
which is a separate follow-up (D-8; below 0.25.0 the key is a compile
error, `V-002`).

```yaml
obligations:
  - { id: "I-1", kind: invariant, text: "No claim exists without provenance.", anchor: "3-1-the-claim" }
  - { id: "I-2", kind: invariant, text: "A claim's predicate is registered at the version it names, and an unknown predicate is refused, never stored as free text.", anchor: "3-4-source-spans" }
  - { id: "I-3", kind: invariant, text: "A registered predicate version is immutable, and no later version changes an existing predicate's value type or cardinality.", anchor: "3-3-epistemic-status-and-relations" }
  - { id: "I-4", kind: invariant, text: "No claim value is a floating-point number.", anchor: "3-2-typed-values" }
  - { id: "I-5", kind: invariant, text: "A source span carries offsets and a keyed digest, never content.", anchor: "3-4-source-spans" }
  - { id: "R-1", kind: requirement, text: "Epistemic status records the source's stance and is never used as aicortex's authority.", anchor: "3-3-epistemic-status-and-relations" }
```

## 10. Amendment of spec 002

Carried by this spec's `amends` edge and effective only when a human
approves this spec. Spec 002's text is not edited.

- **Section 2 (responsibilities).** A tenth responsibility, **claim
  history**: typed temporal claims with provenance, admitted through the
  gate, kept as append-only bitemporal history, and projected as a pure
  function of history, as-of, and policy, for this product and for family
  products that link it as a library (domain `memory`).
- **Section 4 (crate topology).** A row for `aicortex-claims` in the
  admission layer, founded by 050, depending on `aicortex-types` only.
- **Section 5 (build order).** Wave 5 (050 to 059) is family claim history,
  buildable like wave 1 with no network listener. Its exit condition is
  the travel fixture of 052 FR-001 passing in every delivery order inside a
  consumer cell's transaction. `standards/spec/contract.md` names waves 1
  to 4 and is updated to match when this spec is approved (a follow-up in
  spec 001's territory).

## Verification

```verify:cli
# planned: the crate, modules and tests below are this spec's territory and exist once it is built.
cargo test -p aicortex-types --locked
cargo test -p aicortex-claims --locked --test registry
cargo test -p aicortex-store --locked --test predicate_registry
```
