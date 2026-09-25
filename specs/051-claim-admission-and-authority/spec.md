---
id: "051-claim-admission-and-authority"
title: "Claim admission: an observation is not a proposal, a proposal is not a claim, and a score is never authority"
status: approved
kind: "kernel"
domain: "memory"
created: "2026-09-24"
authors: ["Bartek Kus"]
implementation: complete
risk: critical
wave: 5
depends_on:
  - "050-typed-claims-and-predicate-registry"
establishes:
  - "crates/aicortex-types/src/claim_evidence.rs"
  - "crates/aicortex-gate/src/claim_gate.rs"
  - "crates/aicortex-gate/tests/claim_admission.rs"
  - "crates/aicortex-gate/testdata/claims/"
  - "crates/aicortex-store/src/claim_proposal_repo.rs"
  - "crates/aicortex-store/src/claim_admission_repo.rs"
  - "crates/aicortex-store/tests/claim_admission.rs"
extends:
  - { spec: "011-memory-model", unit: "crates/aicortex-types/src/lib.rs", nature: additive }
  - { spec: "013-write-gate-and-redaction", unit: "crates/aicortex-gate/Cargo.toml", nature: additive }
  - { spec: "013-write-gate-and-redaction", unit: "crates/aicortex-gate/src/lib.rs", nature: additive }
  - { spec: "013-write-gate-and-redaction", unit: "crates/aicortex-gate/tests/compile_fail/", nature: additive }
  - { spec: "012-store-schema-and-repositories", unit: "crates/aicortex-store/src/lib.rs", nature: additive }
  - { spec: "012-store-schema-and-repositories", unit: "crates/aicortex-store/src/migrations.rs", nature: additive }
constrains:
  - { flavor: invariant-freeze, unit: "crates/aicortex-gate/src/claim_gate.rs", note: "an admitted claim exists only as the output of the gate; a model score is evidence and never the sole ground of admission" }
references:
  - { unit: { kind: file, path: "specs/013-write-gate-and-redaction/spec.md" }, role: constraint }
  - { unit: { kind: file, path: "specs/014-memory-lifecycle-and-erasure/spec.md" }, role: constraint }
  - { unit: { kind: file, path: "specs/023-review-and-promotion/spec.md" }, role: context }
  - { unit: { kind: file, path: "specs/024-decision-and-audit-references/spec.md" }, role: context }
obligations:
  - { id: "I-1", kind: invariant, text: "No admitted claim exists without provenance and an admission record naming its policy version.", anchor: "3-3-the-gate-and-the-record" }
  - { id: "I-2", kind: invariant, text: "An admitted claim is constructible only by the gate.", anchor: "3-1-three-layers" }
  - { id: "I-3", kind: invariant, text: "A model score never admits a proposal, raises its authority, or lets it supersede another claim.", anchor: "3-2-evidence-and-authority" }
  - { id: "I-4", kind: invariant, text: "A lower-authority proposal never supersedes a higher-authority claim without review.", anchor: "3-3-the-gate-and-the-record" }
  - { id: "I-5", kind: invariant, text: "A correction is appended; it never edits or deletes the corrected claim.", anchor: "3-3-the-gate-and-the-record" }
  - { id: "I-7", kind: invariant, text: "A user correction never overwrites a supplier-sourced claim; a contradiction between them retains both and is surfaced.", anchor: "3-3-the-gate-and-the-record" }
  - { id: "I-6", kind: invariant, text: "Under the initial policy no proposal is admitted without a human review.", anchor: "3-3-the-gate-and-the-record" }
  - { id: "R-2", kind: requirement, text: "The ledger receives one Decision per admission batch listing claim ids, and one per refusal, hold, correction and policy change, never a claim value.", anchor: "3-3-the-gate-and-the-record" }
  - { id: "R-1", kind: requirement, text: "Claim admission is pure and reproducible from the proposal, the registry snapshot, and the policy version.", anchor: "3-3-the-gate-and-the-record" }
summary: >
  Three things that ingest pipelines routinely blur are kept apart: an
  observation (the source as received, stored as a memory through 013's
  gate), a proposal (a typed claim some actor suggests, with its evidence),
  and an admitted claim (a proposal the gate accepted under a named policy,
  with a decision record). Admission reuses the existing gate crate and its
  purity, adds a claim verdict whose `AdmittedClaim` only the gate can mint,
  and records every admission. Model-produced scores are stored as evidence
  and never serve as authority. A user's correction is first-class evidence
  carrying an authority level, and it is appended, never an edit.
---

# 051: Claim admission and authority

## 1. Purpose

An extraction pipeline that writes whatever a model returned straight into
the fact store makes the model the authority on what is true. The first
consumer ingests travel email, where a model will propose a departure time
with a confidence of 0.93 and be wrong in a way that costs a missed flight.
The defect to prevent is not the wrong proposal; it is a store that cannot
say afterwards whether a value was verified against its source, inferred,
or corrected by the traveler, and under which policy it was let in.

Spec 013 already makes admission a pure function that runs before the
transaction and whose output the store's insert path requires by type
(013 B-1, D-5). This spec extends that mechanism to claims rather than
building a second gate, which is what the owner directed (section 7).

## 2. Territory

`claim_evidence.rs` in `aicortex-types` (proposals, evidence, authority
levels). `claim_gate.rs` in `aicortex-gate`, the claim verdict and the
admission policy, extending 013's crate root and its compile-fail suite.
Two repositories in `aicortex-store`: proposals and admission records,
with their migration. The claim history table itself, into which an
admitted claim is appended, is 052's.

## 3. Behavior

### 3.1 Three layers

- **B-1 (observation).** An observation is the source as received: an
  email, a PDF attachment, a calendar file, a user message. It is stored as
  a memory of kind `Observation` through 013's gate, unchanged. Secret
  refusal, size limits, normalization, and origin quarantine apply to it
  exactly as to any memory. A claim's source spans (050 B-12) point into an
  observation.
- **B-2 (proposal).** `ClaimProposal { id, claim: Claim, proposer: Actor,
  evidence: Vec<Evidence>, proposed_at }` is a claim some actor suggests.
  Proposals are appended to `claim_proposal` and are never visible to
  052's projection. A proposal whose source observation is quarantined or
  erased cannot be admitted.
- **B-3 (admitted claim).** An admitted claim is a proposal the gate
  accepted. Only an `AdmittedClaim` value can be appended to claim history
  (052), and only `ClaimVerdict::Admit` produces one, by the same
  type-level construction as 013's `Admitted` (013 D-5).

### 3.2 Evidence and authority

- **B-4 (evidence).** `Evidence` is a closed enum:
  - `SourceSpan(SourceSpan)`: the bytes that support the value;
  - `SpanVerified { span, method }`: a deterministic check re-derived the
    value from the span's bytes (for example, a parser for an IATA
    itinerary block), naming the method and its version;
  - `ModelScore { model: ExtractorVersion, score: Score, label }`: a
    model's confidence, a fixed-point value in `[0, 1]` with the model and
    version that produced it;
  - `RuleMatch { rule, version }`: a deterministic extraction rule fired;
  - `UserStatement { by: Sub, authority: AuthorityLevel }`: a human said
    so, directly or as a correction (B-8);
  - `Corroboration { claims: Vec<ClaimId> }`: other admitted claims that
    agree;
  - `ReviewApproval { by: Sub, decision: DecisionRef }`: a human reviewer
    approved admission of this proposal (B-13). It satisfies review; it
    does not by itself raise the authority level.
  - `OperatorSeed { by: Sub, seed: SeedRef }`: the value belongs to a
    deployment's seed data, loaded deliberately by an operator rather than
    extracted from a source or stated by the scope's owner. `SeedRef` names
    the seed set and its version (B-14, D-8).
  Evidence is stored with the proposal and, on admission, with the
  admission record. It is never discarded when a claim is superseded.
- **B-5 (authority levels).** `AuthorityLevel` is a closed, totally
  ordered enum: `Inferred` < `Extracted` < `Verified` < `UserAsserted` <
  `UserCorrected`. It is assigned by the admission policy from the
  evidence, never supplied by the proposer. A proposal carrying only a
  `ModelScore` is at most `Inferred`; a `RuleMatch` or a model extraction
  with a `SourceSpan` is at most `Extracted`; a `SpanVerified` is at most
  `Verified`; a `UserStatement` from an authenticated human is
  `UserAsserted`, and `UserCorrected` when it targets an existing claim.
  An `OperatorSeed` is at most `Extracted` (B-14): a seed is below every
  user level, so any statement by the scope's owner outranks it.
  An agent actor can never reach a `User*` level (011 B-5's rule about
  agents, applied to authority).
- **B-6 (a score is never authority).** No admission rule may admit a
  proposal, raise its authority level, or let it supersede another claim
  on the strength of a `ModelScore`. A policy may use a score only to order
  a review queue or to refuse a proposal below a floor. A test asserts
  that changing every score in the corpus changes no admitted claim's
  authority and no admit-versus-hold outcome that was not a floor refusal.
- **B-7 (epistemic status is not authority).** A source that says
  `Confirmed` (050 B-10) is a stance recorded in the claim. It does not
  raise the claim's authority level; a model-extracted "confirmed" status
  is still `Extracted` until verified.

### 3.3 The gate and the record

- **B-8 (user corrections).** A correction is a proposal by a human actor
  whose evidence includes `UserStatement` and which names the claim it
  corrects. It is admitted at `UserCorrected` authority and the corrected
  claim's row is unchanged. What relation it carries depends on where the
  corrected claim came from. A claim is **user-sourced** when the evidence
  that admitted it is a `UserStatement` by the owner of its scope, and
  **seed-sourced** when that evidence is an `OperatorSeed` (B-14), and
  **supplier-sourced** otherwise (for travel: a carrier, hotel, or agency
  message, or anything extracted from one).
  - Correcting a user-sourced or seed-sourced claim appends a
    `Supersedes` relation (050 B-11), so the correction replaces the
    earlier value in the view.
  - Correcting a supplier-sourced claim never overwrites it. The
    correction is appended with a `Contradicts` relation, both claims are
    retained, and the conflict is surfaced (052 B-8 reports the slot as
    `Conflicted`, and a review item is raised under 023).
  A correction by an agent is admitted, if at all, at the level its other
  evidence earns and never supersedes a claim of higher authority (B-10).
- **B-9 (the claim verdict).** `Gate::evaluate_claim(&ClaimProposal,
  &RegistrySnapshot, &AdmissionPolicy) -> ClaimVerdict` is pure, like
  `Gate::evaluate` (013 B-10): no clock, no randomness, no network, no store
  read. `ClaimVerdict` is `Admit(AdmittedClaim)`, `Hold(Proposal,
  ClaimReason)` for a proposal that needs review (023), or
  `Refuse(ClaimReason)`. `ClaimReason` is a closed enum of its own, so 013
  B-2's closed `Reason` list is not amended: `UnregisteredPredicate`,
  `InvalidValue` (from 050 B-14), `SecretDetected` (every `Text` value and
  every string qualifier passes 013's detectors), `SourceUnavailable`
  (quarantined or erased observation), `NoProvenance`,
  `AuthorityInsufficient`, `BelowScoreFloor`, and `PolicyDenied`.
- **B-10 (supersession needs authority).** A proposal that carries a
  `Supersedes` relation to a claim of higher authority is held for review,
  never admitted automatically. This is the rule that stops a later,
  lower-authority extraction from overriding a traveler's correction. A
  `Supersedes` relation from a user-sourced proposal to a supplier-sourced
  claim is refused as `PolicyDenied` (B-8: the relation is `Contradicts`).
- **B-11 (every admission is recorded).** An admission writes a
  `claim_admission` row in the same transaction as the claim's append
  (052): the claim id, the proposal id, the policy id and version, the
  authority level assigned, the evidence ids considered, and the verdict.
  The ledger receives (D-4): one Decision per admission batch, listing the
  admitted claim ids, the policy version, and the batch's count, where a
  batch is the set of claims admitted in one transaction; and one Decision
  per refusal, per hold, per user correction, and per policy change. Every
  Decision carries ids, reason codes, and, where 013 B-9 requires one, a
  keyed digest, and never a claim value or source content.
- **B-12 (policy is data and versioned).** `AdmissionPolicy` is a
  versioned document with a digest: the score floor, which evidence
  combinations reach which authority level, and the set of predicates
  qualified for admission without review, each with the minimum authority
  level it requires and the owner decision that qualified it, and the seed
  sets accepted for admission (B-14), each with the owner decision that
  accepted it. A change of
  policy is a new version; an admission record names the version it was
  judged under, so a past admission is reproducible by re-running the gate.
- **B-13 (review before admission).** The initial policy qualifies no
  predicate (D-6): every proposal the gate does not refuse is `Hold`, and
  is admitted only when re-evaluated with a `ReviewApproval` from a human
  reviewer (023). A signed-in principal's `UserStatement` about their own
  data (their profile, preferences, and trips, that is, claims in a scope
  they own) is that principal's review of it: it is admitted at the user
  authority level with no second reviewer (D-7), under B-8's rule for
  supplier-sourced claims. A predicate joins
  the qualified set only after it passes qualification against
  independently labeled data, recorded as a later owner decision, and only
  by a new policy version.
- **B-14 (operator seed).** A proposal whose evidence includes
  `OperatorSeed` is judged as seed data. It is admitted at `Extracted`
  with no `ReviewApproval` only when the policy's accepted seed sets name
  its `SeedRef` exactly (set and version): accepting a seed set is the
  owner's review of every claim in it, the way an own-data statement is its
  principal's review (D-7). A proposal citing a seed set the policy does
  not accept is `Hold`. A proposal that combines `OperatorSeed` with
  `UserStatement` is refused as `PolicyDenied`, so a seed can never be
  dressed as the owner's own word. Seed evidence never reaches a `User*`
  level, never supersedes a claim of higher authority (B-10), and is
  corrected by the scope's owner with `Supersedes` (B-8). The initial
  policy accepts no seed set.

## 4. Functional requirements

- **FR-001.** Every fixture in `aicortex-gate/testdata/claims/` yields its
  recorded verdict, reason, and authority level.
- **FR-002.** A compile-fail test asserts an `AdmittedClaim` cannot be
  constructed outside `aicortex-gate`.
- **FR-003.** B-6's score-perturbation test passes over the whole fixture
  corpus.
- **FR-004.** A proposal whose `Text` value contains a credential is
  refused with `SecretDetected`, and its Decision carries no substring of
  the value (013 FR-002 applied to claims).
- **FR-005.** A traveler's correction of their own seat preference (a
  user-sourced claim) is admitted at `UserCorrected` without a
  `ReviewApproval` and supersedes the earlier preference; the earlier row
  is byte-identical before and after, and a later model extraction of the
  old value is held rather than admitted.
- **FR-010.** A traveler's correction of a departure time taken from a
  carrier email (a supplier-sourced claim) is admitted at `UserCorrected`
  with a `Contradicts` relation, the supplier claim's row is byte-identical
  before and after, both remain in history, a review item is raised, and a
  proposal carrying `Supersedes` from the correction to the supplier claim
  is refused.
- **FR-006.** A proposal citing a quarantined observation is refused with
  `SourceUnavailable`.
- **FR-007.** Re-running the gate over a stored proposal with the policy
  version its admission record names reproduces the recorded verdict and
  authority level.
- **FR-008.** A forced failure on the last statement of an admission
  leaves no claim, relation, admission record, or outbox row (constitution
  XI).
- **FR-009.** Under the initial policy, a proposal with `SpanVerified`
  evidence and a perfect model score is `Hold`; the same proposal
  re-evaluated with a `ReviewApproval` is `Admit` at `Verified`, and an
  admission of three such claims in one transaction appends exactly one
  batch Decision naming the three ids and no value.
- **FR-011.** Under a policy that accepts seed set `S` at version 1, a
  seeded seat preference citing `S` version 1 is admitted at `Extracted`
  without a `ReviewApproval`; the same proposal citing `S` version 2 is
  `Hold`; a proposal carrying both `OperatorSeed` and `UserStatement` is
  refused with `PolicyDenied`; and the traveler's correction of the seeded
  preference is admitted at `UserCorrected` with a `Supersedes` relation,
  the seeded row byte-identical before and after.

## 5. Acceptance criteria

- **AC-1.** `cargo test -p aicortex-gate --locked` passes, including every
  013 fixture unchanged and the new claim corpus.
- **AC-2.** `cargo test -p aicortex-store --locked --test claim_admission`
  passes against rahi's single-voter harness.

## 6. Out of scope

The review queue a held proposal enters (023). The extraction of proposals
from email, which is travel-memory's. Instruction-grade promotion of a
claim, which stays 011's `Promotion` and 023's human act: a claim is at most
`Assertion` or `Evidence` trust, whatever its authority level. Valid time,
supersession ordering, and projection (052).

## 7. Resolved decisions

- **D-1 (2026-09-24, owner).** Observation, proposal, and admitted claim
  are separate layers. Admission goes through the existing gate with a
  decision record.
- **D-2 (2026-09-24, owner).** Model-produced scores are stored as
  evidence and never serve as authority.
- **D-3 (2026-09-24, owner).** User corrections are first-class evidence
  with an authority level.
- **D-4 (2026-09-24, owner decision).** Recorded from the owner's answer
  to this draft's questions: "I agree with all your suggestions and
  recommendations." The ledger records one Decision per admission batch
  listing the claim ids, plus one per refusal, hold, correction, and policy
  change (B-11). The application store keeps the per-claim admission
  record. This resolves Q-1.
- **D-5 (2026-09-24, owner decision).** The five-level authority ladder of
  B-5 is adopted as drafted. The owner's words: "I would like to include it
  but I trust your judgement." This resolves Q-2.
- **D-6 (2026-09-24, owner decision).** The initial admission policy
  admits no predicate without review. Auto-admission is per predicate and
  only after that predicate passes qualification with independently
  labeled data, recorded as a later owner decision (B-12, B-13). Model
  scores can never admit or raise authority (B-6). This resolves Q-3.
- **D-7 (2026-09-24, owner decision).** The owner's answer: "I agree with your recommendations so proceed". A
  signed-in principal's correction to their own profile, preferences, and
  trips counts as their review and is admitted at the user authority
  level, with no second reviewer for own-data corrections. It never
  overwrites supplier-sourced claims: when a user correction contradicts a
  supplier claim, both are retained and a conflict is surfaced (B-8, B-13,
  FR-005, FR-010; 052 B-8). This resolves Q-6.

- **D-8 (2026-09-25, owner decision, operator seed).** The owner's work
  order of 2026-09-25 directs "an `OperatorSeed` evidence/source variant in
  051 ... admitted at an existing authority level; the five-level ladder
  stays unchanged", answering the gap travel-memory's 001 D-7 recorded: a
  deployment's seeded profile, preferences and trips need their own
  provenance, are not `UserAsserted`, and are always outranked by the
  traveler. This amendment, a separate change ahead of the build, adds the
  evidence variant (B-4), its ceiling (B-5), its correction rule (B-8), the
  accepted seed sets in the policy (B-12) and the seed rule (B-14, FR-011).
  The chosen level is `Extracted`, the lowest level that is not a model's
  inference: a seed is deliberate operator data, yet nothing re-derived it
  from source bytes, so `Verified` would overstate it, and every `User*`
  level must stay above it. Acceptance is per seed set and version in the
  versioned policy rather than per claim, because an operator loads a seed
  as one act; a claim-by-claim review of it would add no information.

- **D-9 (2026-09-25, build record).** Choices the spec left open, made
  while building it; none changes what B-1 to B-14 require.
  - *The store's facts.* The pure gate cannot read the store (B-9), yet
    B-2 needs the state of cited sources and B-8, B-10 need the authority
    and sourcing of related claims. They arrive as a fourth argument,
    `ClaimContext`, which the caller reads first, the way 013's `Candidate`
    carries its origin. A source or target the context does not name is
    unavailable: an unnamed source is `SourceUnavailable`, an unnamed
    target is `PolicyDenied` (`unknown_target`), and a relation across
    scopes is `PolicyDenied` (`cross_scope_relation`).
    `ClaimAdmissionRepo::targets` reads the target facts.
  - *Relations.* A proposal carries `ProposedRelation { kind, to }` to
    existing claims; the gate builds each admitted one as a 050
    `ClaimRelation` carrying the claim's provenance, and the admission
    record stores them for 052 to append.
  - *Honoured statements.* A `UserStatement` counts only when the proposer
    is a human whose actor id is the statement's `sub`; any other earns
    nothing (B-5). The level it names is recorded, must be a user level
    (else `PolicyDenied`), and is recomputed: `user_corrected` exactly when
    the proposal relates to a claim by `supersedes` or `contradicts`.
  - *Holds.* A proposal that needs review, and one held by B-10, is held
    with `AuthorityInsufficient`, the closed list's reason for "the
    evidence does not carry the authority to admit unreviewed".
    Evidence that earns no level at all (only corroboration or a review)
    is refused with the same code.
  - *Wire forms.* `Evidence` is `kind`-tagged; `SourceSpan` is written
    `{ "kind": "source_span", "span": ... }`. `Score` is basis points,
    an integer in `[0, 10000]`. `RuleMatch` and `SpanVerified` name their
    rule or method as an `ExtractorVersion` (name and version). An
    evidence item's id is its position in the proposal's list.
  - *Secrets.* The detectors run over a `Text` value, a conditional
    stance's condition, the slot key, and the subject key.
  - *Policy.* `AdmissionPolicy` is an id, a version, an optional score
    floor, per-kind ceilings that may only lower B-5's, qualifications by
    namespace and predicate name (any registered version) with a minimum
    level of at least `extracted`, and accepted seed sets. The initial
    policy is `aicortex.claims.admission` version 1 with none of them. Its
    digest is the SHA-256 of its JSON, computed by the store, which keeps
    each version in an unscoped `admission_policy` table.
  - *Decisions.* Kinds `claims.admission.batch`, `.refuse`, `.hold`,
    `.correction` and `claims.policy.change`. Until 023 exists, the review
    item of B-8 is the correction Decision's `conflicts` list and the
    admission record's `conflicts` column. A refusal's keyed digest is an
    optional argument over `Gate::claim_digest_material`, supplied by the
    caller that holds the key, as in 013.
  - *Rows before 052.* The claim history table is 052's, so FR-005,
    FR-008 and FR-010's rows are, here, the admission record, the proposal
    row, and the outbox row; 052 adds the claim and relation rows to the
    same transaction.
  - *Migration.* Version 5 creates `claim_proposal`, `claim_admission`
    and `admission_policy`; 014's unmerged branch renumbers above it.

## 8. Open questions

Q-1 to Q-3 were resolved by D-4 to D-6, and Q-6 by D-7.

- **Q-4 (proposal retention).** Are proposals kept forever, or erased on a
  retention schedule once admitted or refused? They hold extracted values,
  which are content under constitution XIII.
- **Q-5 (quarantine for claims).** 013 stores a quarantined memory with a
  status. This draft holds a claim as a proposal instead of storing a
  quarantined claim. Is a quarantined-claim status needed for recall?
## 9. Obligations

Declared in the frontmatter `obligations` key (spec-spine 106 grammar),
lifted there from this section once the pin moved to 0.25.0 (050 D-8,
001 D-11).

## Verification

```verify:cli
cargo test -p aicortex-gate --locked
cargo test -p aicortex-store --locked --test claim_admission
```
