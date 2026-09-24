---
id: "048-claim-admission-and-authority"
title: "Claim admission: an observation is not a proposal, a proposal is not a claim, and a score is never authority"
status: draft
kind: "kernel"
domain: "memory"
created: "2026-09-24"
authors: ["Bartek Kus"]
implementation: pending
risk: critical
wave: 4
depends_on:
  - "047-typed-claims-and-predicate-registry"
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

# 048: Claim admission and authority

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
admitted claim is appended, is 049's.

## 3. Behavior

### 3.1 Three layers

- **B-1 (observation).** An observation is the source as received: an
  email, a PDF attachment, a calendar file, a user message. It is stored as
  a memory of kind `Observation` through 013's gate, unchanged. Secret
  refusal, size limits, normalization, and origin quarantine apply to it
  exactly as to any memory. A claim's source spans (047 B-12) point into an
  observation.
- **B-2 (proposal).** `ClaimProposal { id, claim: Claim, proposer: Actor,
  evidence: Vec<Evidence>, proposed_at }` is a claim some actor suggests.
  Proposals are appended to `claim_proposal` and are never visible to
  049's projection. A proposal whose source observation is quarantined or
  erased cannot be admitted.
- **B-3 (admitted claim).** An admitted claim is a proposal the gate
  accepted. Only an `AdmittedClaim` value can be appended to claim history
  (049), and only `ClaimVerdict::Admit` produces one, by the same
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
    agree.
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
  An agent actor can never reach a `User*` level (011 B-5's rule about
  agents, applied to authority).
- **B-6 (a score is never authority).** No admission rule may admit a
  proposal, raise its authority level, or let it supersede another claim
  on the strength of a `ModelScore`. A policy may use a score only to order
  a review queue or to refuse a proposal below a floor. A test asserts
  that changing every score in the corpus changes no admitted claim's
  authority and no admit-versus-hold outcome that was not a floor refusal.
- **B-7 (epistemic status is not authority).** A source that says
  `Confirmed` (047 B-10) is a stance recorded in the claim. It does not
  raise the claim's authority level; a model-extracted "confirmed" status
  is still `Extracted` until verified.

### 3.3 The gate and the record

- **B-8 (user corrections).** A correction is a proposal by a human actor
  whose evidence includes `UserStatement` and which names the claim it
  corrects. On admission it is appended with a `Supersedes` relation (047
  B-11) to the corrected claim at `UserCorrected` authority. The corrected
  claim's row is unchanged. A correction by an agent is admitted, if at
  all, at the level its other evidence earns and never supersedes a claim
  of higher authority (B-10).
- **B-9 (the claim verdict).** `Gate::evaluate_claim(&ClaimProposal,
  &RegistrySnapshot, &AdmissionPolicy) -> ClaimVerdict` is pure, like
  `Gate::evaluate` (013 B-10): no clock, no randomness, no network, no store
  read. `ClaimVerdict` is `Admit(AdmittedClaim)`, `Hold(Proposal,
  ClaimReason)` for a proposal that needs review (023), or
  `Refuse(ClaimReason)`. `ClaimReason` is a closed enum of its own, so 013
  B-2's closed `Reason` list is not amended: `UnregisteredPredicate`,
  `InvalidValue` (from 047 B-14), `SecretDetected` (every `Text` value and
  every string qualifier passes 013's detectors), `SourceUnavailable`
  (quarantined or erased observation), `NoProvenance`,
  `AuthorityInsufficient`, `BelowScoreFloor`, and `PolicyDenied`.
- **B-10 (supersession needs authority).** A proposal that carries a
  `Supersedes` relation to a claim of higher authority is held for review,
  never admitted automatically. This is the rule that stops a later,
  lower-authority extraction from overriding a traveler's correction.
- **B-11 (every admission is recorded).** An admission writes a
  `claim_admission` row in the same transaction as the claim's append
  (049): the claim id, the proposal id, the policy id and version, the
  authority level assigned, the evidence ids considered, and the verdict.
  Every `Refuse` and every `Hold` appends a ledger Decision as 013 B-9
  does, with a keyed digest and never the content. Whether each `Admit`
  also appends a ledger Decision, or only the store record, is open (Q-1).
- **B-12 (policy is data and versioned).** `AdmissionPolicy` is a
  versioned document with a digest: the score floor, which evidence
  combinations reach which authority level, and which predicates require
  `Verified` or above to be admitted without review. A change of policy is
  a new version; an admission record names the version it was judged
  under, so a past admission is reproducible by re-running the gate.

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
- **FR-005.** A traveler's correction of a departure time is admitted at
  `UserCorrected`, the corrected claim's row is byte-identical before and
  after, and a later model extraction of the old value is held rather than
  admitted.
- **FR-006.** A proposal citing a quarantined observation is refused with
  `SourceUnavailable`.
- **FR-007.** Re-running the gate over a stored proposal with the policy
  version its admission record names reproduces the recorded verdict and
  authority level.
- **FR-008.** A forced failure on the last statement of an admission
  leaves no claim, relation, admission record, or outbox row (constitution
  XI).

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
supersession ordering, and projection (049).

## 7. Resolved decisions

- **D-1 (2026-09-24, owner).** Observation, proposal, and admitted claim
  are separate layers. Admission goes through the existing gate with a
  decision record.
- **D-2 (2026-09-24, owner).** Model-produced scores are stored as
  evidence and never serve as authority.
- **D-3 (2026-09-24, owner).** User corrections are first-class evidence
  with an authority level.

## 8. Open questions

- **Q-1 (where an admission is recorded).** B-11 writes an application
  store row for every admission and a ledger Decision for every refusal
  and hold. A ledger Decision per admitted claim would make every travel
  email several chain records. Is the store row sufficient for an
  admission, with ledger Decisions reserved for refusals, holds, user
  corrections, and policy changes?
- **Q-2 (the authority ladder).** Are five levels right, and is
  `UserCorrected` above `UserAsserted`, or equal with the later statement
  winning? Does an operator or a trusted domain rule need a level of its
  own?
- **Q-3 (automatic admission).** Which predicates may a domain admit
  without review at `Extracted`? The draft lets the policy name predicates
  that require `Verified`; the owner may prefer the reverse default.
- **Q-4 (proposal retention).** Are proposals kept forever, or erased on a
  retention schedule once admitted or refused? They hold extracted values,
  which are content under constitution XIII.
- **Q-5 (quarantine for claims).** 013 stores a quarantined memory with a
  status. This draft holds a claim as a proposal instead of storing a
  quarantined claim. Is a quarantined-claim status needed for recall?

## 9. Obligations

Declared in the spec-spine 106 grammar, to be lifted into the frontmatter
`obligations` key when the repository's spec-spine pin reaches 0.25.0
(below it, the key is a compile error, `V-002`).

```yaml
obligations:
  - { id: "I-1", kind: invariant, text: "No admitted claim exists without provenance and an admission record naming its policy version.", anchor: "3-3-the-gate-and-the-record" }
  - { id: "I-2", kind: invariant, text: "An admitted claim is constructible only by the gate.", anchor: "3-1-three-layers" }
  - { id: "I-3", kind: invariant, text: "A model score never admits a proposal, raises its authority, or lets it supersede another claim.", anchor: "3-2-evidence-and-authority" }
  - { id: "I-4", kind: invariant, text: "A lower-authority proposal never supersedes a higher-authority claim without review.", anchor: "3-3-the-gate-and-the-record" }
  - { id: "I-5", kind: invariant, text: "A correction is appended; it never edits or deletes the corrected claim.", anchor: "3-3-the-gate-and-the-record" }
  - { id: "R-1", kind: requirement, text: "Claim admission is pure and reproducible from the proposal, the registry snapshot, and the policy version.", anchor: "3-3-the-gate-and-the-record" }
```

## Verification

```verify:cli
# planned: the gate module, fixtures and store tests below exist once this spec is built.
cargo test -p aicortex-gate --locked
cargo test -p aicortex-store --locked --test claim_admission
```
