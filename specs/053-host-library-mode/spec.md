---
id: "053-host-library-mode"
title: "Host-library mode: named migrations, operator-authorized vocabulary, and one caller-owned transaction"
status: approved
kind: kernel
domain: memory
created: "2026-09-26"
authors: ["Bartek Kus"]
implementation: in-progress
risk: critical
wave: 5
depends_on:
  - "014-memory-lifecycle-and-erasure"
  - "050-typed-claims-and-predicate-registry"
  - "051-claim-admission-and-authority"
  - "052-bitemporal-claim-history-and-projection"
establishes:
  - "crates/aicortex-store/src/host.rs"
  - "crates/aicortex-store/tests/host_library.rs"
  - "fixtures/external-host/Cargo.toml"
  - "fixtures/external-host/src/lib.rs"
  - "fixtures/external-host/tests/atomic.rs"
extends:
  - { spec: "010-chassis-adoption-and-workspace", unit: { kind: section, file: "Cargo.toml", anchor: "workspace" }, nature: additive }
  - { spec: "010-chassis-adoption-and-workspace", unit: "apps/aicortex/src/cell.rs", nature: additive }
  - { spec: "010-chassis-adoption-and-workspace", unit: "apps/aicortex/tests/migrate.rs", nature: additive }
  - { spec: "012-store-schema-and-repositories", unit: "crates/aicortex-store/src/lib.rs", nature: additive }
  - { spec: "012-store-schema-and-repositories", unit: "crates/aicortex-store/src/migrations.rs", nature: additive }
  - { spec: "012-store-schema-and-repositories", unit: "crates/aicortex-store/src/memory_repo.rs", nature: additive }
  - { spec: "012-store-schema-and-repositories", unit: "crates/aicortex-store/tests/schema.rs", nature: additive }
  - { spec: "014-memory-lifecycle-and-erasure", unit: "crates/aicortex-store/src/erasure.rs", nature: additive }
  - { spec: "014-memory-lifecycle-and-erasure", unit: "crates/aicortex-store/tests/erasure.rs", nature: additive }
  - { spec: "050-typed-claims-and-predicate-registry", unit: "crates/aicortex-store/src/predicate_registry_repo.rs", nature: additive }
  - { spec: "050-typed-claims-and-predicate-registry", unit: "crates/aicortex-store/tests/predicate_registry.rs", nature: additive }
  - { spec: "051-claim-admission-and-authority", unit: "crates/aicortex-store/src/claim_proposal_repo.rs", nature: additive }
  - { spec: "051-claim-admission-and-authority", unit: "crates/aicortex-store/src/claim_admission_repo.rs", nature: additive }
  - { spec: "051-claim-admission-and-authority", unit: "crates/aicortex-store/tests/claim_admission.rs", nature: additive }
  - { spec: "052-bitemporal-claim-history-and-projection", unit: "crates/aicortex-store/src/claim_repo.rs", nature: additive }
  - { spec: "052-bitemporal-claim-history-and-projection", unit: "crates/aicortex-store/tests/claim_history.rs", nature: additive }
  - { spec: "052-bitemporal-claim-history-and-projection", unit: "crates/aicortex-store/tests/claim_erasure.rs", nature: additive }
  - { spec: "052-bitemporal-claim-history-and-projection", unit: "crates/aicortex-claims/tests/projection.rs", nature: additive }
amends:
  - "050-typed-claims-and-predicate-registry"
amends_sections:
  - "8-open-questions"
constrains:
  - { flavor: invariant-freeze, unit: "crates/aicortex-store/src/host.rs", note: "the library migration set is named aicortex, carries migrations 1 through 8 byte for byte, and requires rahi.receipts and rahi.coordination at version 1 or later" }
  - { flavor: invariant-freeze, unit: "crates/aicortex-store/src/predicate_registry_repo.rs", note: "only explicit operator configuration authorizes predicate registration; domain input, model output, and caller-selected subjects do not" }
  - { flavor: invariant-freeze, unit: "crates/aicortex-store/src/claim_repo.rs", note: "the host owns the TxnBuilder and the only commit; aicortex repositories stage and never commit host work" }
references:
  - { unit: { kind: file, path: "specs/050-typed-claims-and-predicate-registry/spec.md" }, role: constraint }
  - { unit: { kind: file, path: "specs/051-claim-admission-and-authority/spec.md" }, role: constraint }
  - { unit: { kind: file, path: "specs/052-bitemporal-claim-history-and-projection/spec.md" }, role: constraint }
obligations:
  - { id: "I-1", kind: invariant, text: "The library migration set has the immutable name aicortex and preserves every migration version, SQL byte, checksum, name, and additive declaration from the standalone app sequence.", anchor: "3-1-one-named-migration-set" }
  - { id: "I-2", kind: invariant, text: "Predicate registration is authorized only by an explicit operator configuration grant over an exact namespace, version, and canonical document digest.", anchor: "3-2-operator-authorized-predicate-registration" }
  - { id: "I-3", kind: invariant, text: "Aicortex stages writes into the host-owned TxnBuilder and never opens or commits an independent transaction on the host path.", anchor: "3-3-one-caller-owned-transaction" }
  - { id: "I-4", kind: invariant, text: "Every claim-history read remains bounded by scope, subject, and an explicit as-of on both transaction and valid-time axes.", anchor: "3-4-bounded-reads-and-pure-projection" }
  - { id: "R-1", kind: requirement, text: "An external host fixture proves one Rahi receipt or work transition, one source observation, one admitted claim-history write, one host projection revision, and one outbox intent commit or roll back together.", anchor: "3-5-external-host-proof" }
summary: >
  Expose Aicortex as a linked library inside another Rahi cell without giving
  up migration identity, operator authority, or transaction ownership. The
  library exports the immutable named migration set `aicortex`, makes
  predicate registration an explicit operator-configured act, and stages
  observations, admission records, claim history, erasure records, and
  outbox work into the caller's `rahi_store::TxnBuilder`. A separate host
  fixture proves that Rahi ingress state, Aicortex history, a host-owned
  projection revision, and outbox work share one commit and one rollback.
---

# 053: Host-library mode

## 1. Purpose

Specs 050 to 052 deliberately made Aicortex repositories stage into a
caller's Rahi transaction so a product can link the claim-history capability
inside its own cell. Two contracts remained outside the approved corpus:
the immutable migration-set identity supplied by released Rahi spec 046, and
the operator authority that closes spec 050 Q-2. Without those contracts a
host must either renumber Aicortex migrations, treat a domain payload as
authority to install vocabulary, or split one ingest operation across
several commits.

This spec defines the host seam and no product domain. It preserves
constitution VI, VIII, IX, and XI: Rahi remains the chassis and transaction
owner, every claim keeps provenance, and a linked library does not become a
second store, ledger, identity system, or scope model.

## 2. Territory

`aicortex-store` gains `host.rs`, which owns the named migration-set factory,
the operator predicate-registration types, and the public host contract.
Existing repositories are extended only where a host-facing staging seam is
missing or an existing seam must require the new authorization value. The
standalone `apps/aicortex` cell keeps its existing `app` migrations and
behavior.

The future `fixtures/external-host` package is a small Rahi cell distinct
from the Aicortex application binary. It exists only to prove library
composition and is not a travel adapter or a second shipping application.

Operator prerequisite: Rahi `v0.4.0`, signed and peeling to
`e845bd08f3b73a17f19656accb3c2ab24975f277`, carries approved and complete
Rahi spec 046. Its released API supplies `MigrationSet`, `SetName`,
`SetRequirement`, `Cell::migration_sets`, `TxnBuilder`, receipt and work
staging, and outbox staging. Implementation stops if the exact released API
does not satisfy this contract.

## 3. Behavior

### 3.1 One named migration set

- **B-1 (immutable identity).** `aicortex_store::AICORTEX_MIGRATION_SET_NAME`
  is `"aicortex"`. `aicortex_store::migration_set() -> Result<MigrationSet,
  Error>` returns exactly one set with that name. A host cannot supply or
  override the name.
- **B-2 (byte-identical history).** The set carries the eight entries
  returned by the existing `aicortex_store::migrations()` in the same order,
  with versions 1 through 8, names, SQL bytes, checksums, and additive flags
  unchanged. The library does not copy, rewrite, offset, or renumber SQL.
  Migration identity in a host is therefore `(aicortex, version)` while the
  standalone application's existing identity remains `(app, version)`.
- **B-3 (requirements).** The set declares `rahi.receipts >= 1` and
  `rahi.coordination >= 1` with Rahi's released `MigrationSet::requires`
  API. No weaker requirement and no host-selected substitute is accepted.
- **B-4 (standalone compatibility).** `apps/aicortex::Aicortex` continues to
  return `aicortex_store::migrations()` from `Cell::migrations()` and does
  not also return the `aicortex` set from `Cell::migration_sets()`. Existing
  standalone stores therefore retain their byte-identical `app` history and
  schema behavior. A host cell links `aicortex_store::migration_set()` from
  its own `Cell::migration_sets()`.

### 3.2 Operator-authorized predicate registration

- **B-5 (authorization input).** `OperatorPredicateConfig` is the only
  authorization input. It contains the authenticated operator `Sub` and a
  non-empty list of `PredicateRegistrationGrant { namespace, version,
  document_digest }`. Each grant names one exact canonical predicate-set
  document. A grant is host startup or migration configuration, not a claim,
  domain payload, tenant field, model result, HTTP parameter, or
  caller-selected subject.
- **B-6 (validation result).** `PredicateRegistryRepo::validate_registration(
  snapshot, set, config) -> Result<PredicateRegistrationPlan,
  PredicateRegistrationRefusal>` is pure and stages nothing.
  `PredicateRegistrationPlan` is a closed enum:
  `Insert(AuthorizedPredicateRegistration)` or
  `Unchanged(PredicateRegistrationIdentity)`. The authorized insertion value
  is opaque outside `aicortex-store` and records the operator, namespace,
  version, canonical document digest, and canonical document bytes.
  `PredicateRegistrationRefusal` distinguishes at least
  `NotOperatorGranted`, `GrantDigestMismatch`, `ConflictingVersion`, and the
  existing registry validation failures.
- **B-7 (canonical identity and idempotence).** The document digest is
  `sha256:` plus lowercase SHA-256 over the exact canonical JSON bytes the
  repository stores. If the registry already holds the same namespace,
  version, and digest, validation returns `Unchanged`, stages no statement,
  appends no Decision, and writes no registry row. If the namespace and
  version exist with another digest, or the grant's digest differs from the
  document, validation refuses before any statement is staged.
- **B-8 (authorized staging).** `PredicateRegistryRepo::stage_registration(
  txn, plan, at) -> Result<Registration, Error>` consumes the validated plan.
  It stages an insert and returns the existing registration `LedgerEntry`
  only for `Insert`; `Unchanged` returns `Registration::Unchanged` without
  modifying `txn`. The current public path that accepts a free `registrant:
  &Sub` is replaced by this authorized path. No public staging API accepts a
  domain-selected registrant as authority.

### 3.3 One caller-owned transaction

- **B-9 (the host owns commit).** The host constructs one
  `rahi_store::TxnBuilder`, calls Rahi and Aicortex staging APIs, and invokes
  `StoreHandle::txn` exactly once. Aicortex host APIs take `&mut TxnBuilder`,
  return values needed after commit, and never call `execute`, `txn`,
  `fenced_txn`, or an equivalent commit operation.
- **B-10 (complete staging surface).** The supported host surface is:
  `MemoryRepo::insert` for a gated source observation and its outbox work;
  `ClaimProposalRepo::stage_append`; `ClaimAdmissionRepo::stage_admit` and
  `stage_policy`; `ClaimRepo::stage_append`, `stage_retraction`, and
  `stage_erase`; the authorized predicate-registration path of B-6 to B-8;
  and Rahi's `Outbox::stage`. `ClaimRepo::stage_append` continues to stage
  claim rows, relations, per-scope history counters, admission records, and
  Aicortex outbox work together.
- **B-11 (missing erasure seam).** Memory and scope erasure currently own
  their transaction inside `Eraser`; that is the missing public host staging
  seam. Implementation adds
  `Eraser::prepare_erase(store, scope, id, cascade, authority, at) ->
  Result<PreparedErasure, Error>` and
  `Eraser::stage_erase(txn, prepared) -> Result<Erased, Error>`.
  `PreparedErasure` is opaque, includes the compare-at-commit guards and
  durable erasure receipt work the current path requires, and can be consumed
  once. The standalone `Eraser::erase` becomes composition over those calls
  and one commit. Multi-batch scope erasure keeps its lease and batch commits;
  this spec does not pretend a multi-transaction drain fits into one host
  ingest transaction.
- **B-12 (decisions do not split SQL atomicity).** Admission, registration,
  correction, and erasure staging may return ledger entries or durable
  receipts for the caller to deliver through the established Rahi recovery
  path after SQL commit. No claim value or source content enters the ledger,
  and a ledger append is not misrepresented as part of the SQL transaction.

### 3.4 Bounded reads and pure projection

- **B-13 (bounded reads).** Every history read continues to require `Scope`,
  `SubjectRef`, and `TxBound`; projection continues to require an explicit
  `AsOf` on the transaction and valid-time axes plus a `ProjectionPolicy`.
  Host mode adds no unscoped, subject-optional, unbounded, or implicit-now
  convenience read.
- **B-14 (host projection is host data).** The host may stage its own
  projection revision in the same transaction. That row belongs to the host
  and is derived from `aicortex_claims::project`. It does not add an Aicortex
  materialized view or cache, does not become projection input, and does not
  change 052's compute-on-read contract.

### 3.5 External-host proof

- **B-15 (external fixture).** `fixtures/external-host` links the public
  Aicortex crates and released Rahi crates as libraries. It does not depend on
  `apps/aicortex`, use private Aicortex modules, add travel types, or copy SQL.
  Its cell supplies `rahi.coordination`, `rahi.receipts`, and
  `aicortex_store::migration_set()` through `Cell::migration_sets()`.
- **B-16 (one atomic unit).** The fixture stages in one host-owned builder:
  a Rahi inbound receipt or work transition, a gated source observation, its
  proposal and admitted claim history with relations and admission record, a
  host-owned projection revision, and an outbox intent. One
  `StoreHandle::txn` commits them. Deterministic failure on the last statement
  proves every one rolls back, including Rahi ingress state and both host and
  Aicortex rows.
- **B-17 (restart and no-op apply).** A fresh store applies all three named
  sets, restarts with identical durable state, validates every Aicortex
  checksum, and applies the plan again. The second apply records no schema
  row and an identical predicate registration writes no predicate-registry
  row. A changed migration checksum or changed predicate document digest is
  refused before writes.

## 4. Functional requirements

- **FR-001.** The `aicortex` set has exactly versions 1 through 8. A fixture
  records the exact SHA-256 checksum, name, and additive flag of every entry
  and proves each equals the corresponding entry of `migrations()`.
- **FR-002.** The set has exactly the two requirements
  `rahi.receipts >= 1` and `rahi.coordination >= 1`; removing either makes the
  contract test fail.
- **FR-003.** Standalone migration tests pass with their existing `app`
  history unchanged. A host migration records Aicortex only under set name
  `aicortex`, and a second host migration applies nothing.
- **FR-004.** Exact operator configuration admits one predicate document.
  Repeating the same namespace, version, and canonical digest is a no-op with
  an unchanged transaction length and row count. Changing either the grant
  digest or the stored document digest returns the typed refusal before the
  builder gains a statement.
- **FR-005.** A domain payload, tenant input, model output, and a
  caller-selected `Sub` each fail to produce an
  `AuthorizedPredicateRegistration`; compile-fail or public-API tests prove
  no alternate constructor or free-registrant staging path exists.
- **FR-006.** The external host fixture's successful path observes exactly
  one durable Rahi receipt or work transition, source observation, admitted
  claim and admission record, host projection revision, and outbox intent
  after restart.
- **FR-007.** Deterministic failure injection at the fixture transaction's
  last statement leaves zero rows for every item in FR-006 and no advanced
  Rahi receipt or work state.
- **FR-008.** Fresh migration, checksum validation, restart, and a second
  identical apply succeed; the second apply adds zero
  `schema_set_version` rows and zero predicate-registry rows. An altered
  checksum is refused as integrity failure before any set is advanced.
- **FR-009.** Existing store, migration, claim-admission, claim-history,
  erasure, and projection suites pass unchanged in intent. Reads remain
  scope-, subject-, and explicit-as-of-bounded.

## 5. Acceptance criteria

- **AC-1.** `cargo test -p aicortex-store --locked --test host_library`
  passes the named-set identity, exact checksum, requirement, authorization,
  refusal, and idempotence cases.
- **AC-2.** `cargo test --manifest-path fixtures/external-host/Cargo.toml
  --locked --test atomic` passes the all-commit, all-rollback, restart, and
  second-apply cases against a real single-voter Rahi store.
- **AC-3.** The existing standalone migration, store, admission, history,
  erasure, and projection regressions pass.
- **AC-4.** The full repository governance and Rust gates pass with no waiver.

## 6. Out of scope

Travel predicates, a tm adapter, HTTP or MCP changes, provider calls, a new
receipt or work system, another ledger, another identity or scope model, and
another transaction owner are out of scope. Aicortex crate publication is
also out of scope. The downstream handoff after implementation is one exact
merged Git revision, the `aicortex` migration-set identity and checksums, the
public host API, the resolved dependency tree, and the external-host evidence.

## 7. Resolved decisions

- **D-1 (2026-09-26, Rahi 046 and Aicortex 050 D-9, D-10).** Library
  migrations use the immutable set name `aicortex`, keep versions 1 through
  8 byte for byte, and require `rahi.receipts >= 1` and
  `rahi.coordination >= 1`. The standalone app keeps its `app` history.
  Rejected: a host offset, host-selected name, copied SQL, or renumbering.
- **D-2 (2026-09-26, Aicortex 050 Q-2 and travel-memory decision packet).**
  Predicate registration is a startup or migration act authorized by explicit
  operator configuration over an exact namespace, version, and canonical
  digest. Rejected: first-writer authority, a tenant or domain payload, model
  output, and a caller-selected subject. This draft proposes the answer to
  Q-2; it does not ratify it.
- **D-3 (2026-09-26, Aicortex 050 D-4, 052 B-13, and the travel-memory
  decision packet).** The host owns the builder and the single commit.
  Aicortex stages observations, proposals, admissions, claims, relations,
  counters, erasure records, and outbox work. Rejected: an Aicortex-owned
  transaction nested inside host ingestion.
- **D-4 (2026-09-26, Aicortex 052 D-8 and this dispatch).** The external
  fixture's projection revision is host-owned evidence of atomic composition,
  not an Aicortex materialized cache. Aicortex projection remains pure and
  compute-on-read.
- **D-5 (2026-09-26, source review).** Existing claim and observation paths
  already stage into a caller's `TxnBuilder`. The missing public seam is the
  memory erasure path, whose `Eraser` currently creates and commits its own
  transaction. The implementation must split preparation from staging while
  preserving compare-at-commit guards and durable recovery.
- **D-6 (2026-09-26, draft authority boundary).** This file is a proposed
  contract authored under dispatch `aicortex-library-mode-spec-001`. Owner
  ratification remains explicitly open. `status: draft` and
  `implementation: pending` remain until Bart separately approves the
  contract; no implementation or downstream consumption is authorized by
  this draft.
- **D-7 (2026-09-26, owner ratification).** Bart approved spec 053
  completely and authorized its end-to-end build, shipment, and shepherding.
  This is the human ratification D-6 reserved. The spec moves to
  `status: approved`; implementation remains a separately evidenced act and
  must satisfy every acceptance criterion before it moves to complete.

## Verification

```verify:cli
cargo test -p aicortex-store --locked --test host_library
cargo test --manifest-path fixtures/external-host/Cargo.toml --locked --test atomic
cargo test -p aicortex-store --locked --test schema
cargo test -p aicortex-store --locked --test predicate_registry
cargo test -p aicortex-store --locked --test claim_admission
cargo test -p aicortex-store --locked --test claim_history
cargo test -p aicortex-store --locked --test claim_erasure
cargo test -p aicortex-store --locked --test erasure
cargo test -p aicortex-claims --locked --test projection
cargo test -p aicortex-claims --locked --test reorder
cargo test -p aicortex --locked --test migrate
make spine
make ci
```
