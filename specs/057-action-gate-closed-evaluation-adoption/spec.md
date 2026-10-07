---
id: "057-action-gate-closed-evaluation-adoption"
title: "Sequence the write gate and claim admission with action-gate's closed evaluation mode"
status: draft
kind: "feature"
domain: "memory"
created: "2026-10-07"
authors: ["Bartek Kus"]
implementation: in-progress
risk: high
wave: 5
depends_on:
  - "013-write-gate-and-redaction"
  - "047-shared-secret-detector-convergence"
  - "051-claim-admission-and-authority"
establishes:
  - "crates/aicortex-gate/src/walk.rs"
  - "crates/aicortex-gate/tests/differential.rs"
extends:
  - { spec: "013-write-gate-and-redaction", unit: "crates/aicortex-gate/src/lib.rs", nature: additive }
  - { spec: "051-claim-admission-and-authority", unit: "crates/aicortex-gate/src/claim_gate.rs", nature: additive }
  - { spec: "010-chassis-adoption-and-workspace", unit: "Cargo.toml", nature: additive }
references:
  - { unit: { kind: file, path: "specs/013-write-gate-and-redaction/spec.md" }, role: constraint }
  - { unit: { kind: file, path: "specs/047-shared-secret-detector-convergence/spec.md" }, role: context }
  - { unit: { kind: file, path: "specs/051-claim-admission-and-authority/spec.md" }, role: constraint }
obligations:
  - { id: "I-1", kind: invariant, text: "Every candidate and every claim proposal receives the verdict, reason and payload the pre-057 evaluators gave it.", anchor: "3-1-what-moves-and-what-stays" }
  - { id: "I-2", kind: invariant, text: "Every rule of 013 and 051 is a required check, so a walk without one of them refuses.", anchor: "3-2-the-walk" }
  - { id: "R-1", kind: requirement, text: "aicortex-gate pins action-gate-core at exactly 0.3.0 with default features off, and no action-gate-core type passes between aicortex and rahi.", anchor: "3-3-the-pin" }
summary: >
  aicortex-gate sequences its two questions, 013's write gate and 051's
  claim admission, with hand-written chains of early returns. action-gate
  0.3.0 (its spec 004) adds a closed evaluation mode written for exactly
  this consumer: deny by default, required checks, a deny that ends the
  walk, and a degrade that does not. This spec moves the sequencing onto
  that mode, one required check per rule in the documented order, and
  keeps every aicortex type, payload, admission token and ledger entry in
  aicortex-gate as adapter code. Behaviour does not change, and a
  differential test against the replaced evaluators shows it.
---

# 057: action-gate closed evaluation adoption

## 1. Purpose

013 and 051 each document an order of evaluation and implement it as a
chain of early returns in `Gate::evaluate` and `Gate::evaluate_claim`.
Three of their properties then rest on that chain being written correctly
and staying so: the size ceiling precedes the scan (013 B-6), the
detectors precede the quarantine (013 B-4, B-7), and nothing can turn a
rule off (013 B-5). action-gate-core 0.3.0, released under action-gate
spec `004-closed-evaluation-mode`, makes each of them a rule of the gate
instead: a deny ends a closed walk (004 B-4), a degrade does not and every
deny outranks it (004 B-5, B-7), and a required check that is not
registered denies before anything runs (004 B-3). Section 6.2 of that spec
is this migration's plan. The family rule of 047 (consume, never fork)
applies as it did to the detectors.

## 2. Territory

`crates/aicortex-gate`: a new private module `walk.rs` (the closed walk
and its adapter), `Gate::evaluate` and its rules in `lib.rs` (013),
`Gate::evaluate_claim` and its rules in `claim_gate.rs` (051), and the
differential test `tests/differential.rs`. In the workspace manifest, the
`action-gate-core` pin (010, 047). Nothing in action-gate or rahi.

## 3. Behavior

### 3.1 What moves and what stays

- **B-1 (what moves).** The sequencing of both questions: which rule runs
  next, what ends the evaluation, and how an admission, a quarantine or
  hold, and a refusal combine. It is action-gate-core's closed mode.
- **B-2 (what stays).** Everything 004 section 6.2 lists as adapter code:
  `Reason`, `ClaimReason`, `Verdict`, `ClaimVerdict`, `Admitted` and
  `AdmittedClaim` with their crate-private constructors (013 B-1, 051
  I-2), `Override` and `evaluate_overridden` (013 B-5), normalization
  (013 B-8), `LedgerEntry` and every entry builder (013 B-9, 051 B-11),
  building an `AdmittedClaim` after the walk admits (relations, conflicts
  and corrections), and the map from the deciding check back to a reason
  with its payload. Each rule's predicate stays in aicortex-gate, as one
  step.
- **B-3 (no behaviour change).** For every input, the verdict, its reason
  with every payload field (counts, offsets, detector, field, rule name,
  source), the admitted or quarantined memory, the held proposal and the
  admitted claim equal what the pre-057 evaluators returned. The committed
  corpora of 013 and 051, the golden vectors of 047 and every existing test
  pass unchanged.

### 3.2 The walk

- **B-4 (one required check per rule).** Each evaluation builds one closed
  gate whose checks are the rules of the owning spec, registered in the
  documented order and each required: for 013 the ids of `CAPTURE_STEPS`
  (`capture.empty`, `capture.too_large`, `capture.media`,
  `capture.denied_source`, `capture.secrets`, `capture.origin`), and for
  051 those of `CLAIM_STEPS` (`claim.vocabulary`, `claim.secrets`,
  `claim.provenance`, `claim.sources`, `claim.seed`, `claim.score_floor`,
  `claim.user_level`, `claim.authority`, `claim.relations`,
  `claim.ground`). Normalization (013 step 1) precedes the walk.
- **B-5 (what a step answers).** A step allows when it finds nothing wrong,
  denies to refuse, and degrades to quarantine or hold. Only
  `capture.origin` (asserted origin) and `claim.ground` (outranked or
  ungrounded) degrade. Every step decides on every input, so no required
  step abstains.
- **B-6 (the three properties).** (a) A refusal ends the walk, so an
  oversized body is never scanned. (b) A quarantine or hold never ends the
  walk, and any refusal outranks it wherever the degrading step is
  registered. (c) A walk missing any required step refuses with
  `gate:deny:closed:required_unregistered`, so no assembly that drops the
  detectors admits.
- **B-7 (the walk's own refusals).** A refusal the gate makes itself
  (`required_unregistered`, `required_undecided`, `no_check_decided`), or
  a deciding step that does not confirm its decision when judged again
  (`aicortex:deny:walk:inconsistent`), is a refusal:
  `Reason::PolicyDenied { policy }` for 013 and
  `ClaimReason::PolicyDenied { rule }` for 051, naming that code. None of
  them is reachable from the production walks; they are mapped so that the
  adapter has no admitting default.

### 3.3 The pin

- **B-8 (exact pin, pure path).** `action-gate-core` moves from `=0.2.0`
  to `=0.3.0`, default features off, so `regex` stays out of
  aicortex-gate's path (047 D-4). 0.3.0's secret registry, its golden
  vectors and its open mode are those of 0.2.0 (action-gate 004 B-9,
  B-11), and the walk is pure: no clock, randomness, network or float
  (013 B-10, 051 R-1).
- **B-9 (the chassis's copy).** rahi 0.4.0's `rahi-kernel` pins
  `action-gate-core =0.2.0`. The two versions are semver-incompatible, so
  the lockfile carries both. This is admissible only while no
  `action-gate-core` type passes between aicortex and rahi (section 7,
  D-3). `action-gate-types` stays one crate at 0.1.0.

## 4. Functional requirements

- **FR-001.** Every existing test of aicortex-gate passes unchanged: 013's
  corpus, 047's registry parity, 051's claim corpus, the end-to-end
  capture tests, and the compile-fail cases.
- **FR-002.** A differential test runs the pre-057 evaluators, reproduced
  over the public API as an oracle, against the walk: 013's corpus under
  five rule sets and four origins, and 051's corpus varied one axis at a
  time and then sampled across every axis with a fixed seed. The verdicts
  are equal, the reasons serialize to equal bytes, and the run reaches
  every verdict, every reason code, every 051 rule name, and inputs that
  break several rules at once.
- **FR-003.** Unit tests over the production steps hold B-6: the
  detectors do not run on an oversized body; a credential under an
  asserted origin is refused with origin registered last or first; and
  dropping any one step refuses a candidate the full walk admits.
- **FR-004.** The registered order of each walk equals its public step
  list.

## 5. Acceptance criteria

- **AC-1.** `cargo test -p aicortex-gate --locked` passes.
- **AC-2.** `cargo test -p aicortex-gate --locked --test differential`
  passes.
- **AC-3.** `cargo deny check` passes with both `action-gate-core`
  versions in the tree.
- **AC-4.** `make ci` passes.

## 6. Out of scope

Changing action-gate or rahi. Moving any aicortex type into action-gate.
Adopting the gate's `config_hash` as an identity: the walk's hash names
the step list, and 051's `PolicyRef` stays the policy's identity.
`evaluate_exhaustive`: both questions answer with their first refusal, as
before.

## 7. Resolved decisions

These were taken by the agent drafting this spec. The owner ratifies them
by approving it.

- **D-1 (2026-10-07; one gate per evaluation, not per `RuleSet`).**
  action-gate's checks are `'static` and read only an `ActionContext`,
  whose fields are strings and JSON values. Carrying a `Candidate` or a
  `ClaimProposal` through it would mean serializing and parsing the input
  in every step. Each evaluation therefore builds its gate, and each step
  holds a shared, owned copy of the subject: the normalized body, source,
  origin and rules for 013; the proposal, registry snapshot, policy,
  context and detector rules for 051. aicortex's `Gate` keeps its derives
  (`Clone`, `PartialEq`, `Eq`, `Default`), which an embedded
  `action_gate_core::Gate` would not allow. The cost is a clone of those
  inputs per evaluation.
- **D-2 (2026-10-07; recovering the payload).** A `Decision` carries no
  payload (action-gate 001 D-4, 004 D-7). The deciding step is named by
  the decision's `check_ids`, and the adapter judges that step once more
  on the same subject to recover its reason. Steps are pure, so the
  second judgement equals the first; a refused credential is scanned
  twice. Rejected: interior mutability in the checks, and a reason
  encoded in the decision's string, which `Reason` cannot be parsed back
  from (its detector is a `&'static str`).
- **D-3 (2026-10-07; the duplicate core).** After the bump the lockfile
  has `action-gate-core` 0.2.0 (through `rahi-kernel` 0.4.0) and 0.3.0
  (through aicortex-gate). aicortex-gate is the only aicortex crate that
  depends on `action-gate-core`, it does not depend on `rahi-kernel`, and
  no aicortex crate calls `rahi_kernel::build_gate` or `Manifest::gate`,
  the two public items of `rahi-kernel` that return a 0.2.0 `Gate`. The
  `ActionContext` and `Outcome` rahi re-exports are `action-gate-types`
  0.1.0, one crate in both graphs. Nothing crosses, so the duplicate is
  accepted, as 047 D-7 accepted the 0.1.0 copy: `action-gate-core` is not
  in `deny.toml`'s refuse-duplicates list, and it converges when rahi
  moves its pin.
- **D-4 (2026-10-07; step ids).** Step ids are namespaced by question
  (`capture.`, `claim.`) and never appear in a verdict, a reason or a
  ledger entry, so naming them changes no stored byte.
- **D-5 (2026-10-07; ordinal).** 055 and 056 are taken by an open pull
  request, so this spec is 057.
- **D-6 (2026-10-07; two pull requests).** The change lands as a stack so
  each part stays reviewable: first this spec and `tests/differential.rs`,
  whose oracle then runs against the evaluators it reproduces and so shows
  that the reproduction is faithful; then the pin, `walk.rs` and the
  switch of both questions onto it, under the same unchanged test.

## 8. Follow-ups and open questions

- **F-1 (rahi).** When rahi moves `rahi-kernel` to `action-gate-core`
  0.3.0 or later, the lockfile returns to one copy; nothing here needs to
  change.

## Verification

```verify:cli
cargo test -p aicortex-gate --locked
cargo test -p aicortex-gate --locked --test differential
```
