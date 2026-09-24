---
id: "046-rahi-0-2-0-adoption"
title: "Move every chassis pin to rahi 0.2.0 together, and declare which migrations are additive"
status: draft
kind: "tooling"
domain: "chassis"
created: "2026-09-24"
authors: ["Bartek Kus"]
implementation: pending
risk: high
wave: 4
depends_on:
  - "013-write-gate-and-redaction"
extends:
  - { spec: "010-chassis-adoption-and-workspace", unit: { kind: section, file: "Cargo.toml", anchor: "workspace.dependencies" }, nature: additive }
  - { spec: "010-chassis-adoption-and-workspace", unit: "deny.toml", nature: additive }
  - { spec: "010-chassis-adoption-and-workspace", unit: "apps/aicortex/tests/cell.rs", nature: additive }
  - { spec: "012-store-schema-and-repositories", unit: "crates/aicortex-store/src/migrations.rs", nature: additive }
  - { spec: "012-store-schema-and-repositories", unit: "apps/aicortex/tests/migrate.rs", nature: additive }
references:
  - { unit: { kind: file, path: "specs/010-chassis-adoption-and-workspace/spec.md" }, role: constraint }
  - { unit: { kind: file, path: ".github/dependabot.yml" }, role: context }
summary: >
  The nine rahi crates are pinned at `=0.1.0`. rahi 0.2.0 is published on
  crates.io for all nine and is a minor release that changes the consumer
  contract: migrations gain an `additive` declaration and the store records a
  checksum per applied version, restore and first-boot export change their
  Rust API, backups authenticate with a dedicated passkey, and the ledger gains
  lifetime decision identity. This spec moves all nine pins in one change,
  keeps the exact-pin rule of spec 010, decides for each shipped migration
  whether it is additive, and re-runs spec 010's supply-chain revisit against
  the new tree. It must land before any claim-history migration (050 to 052)
  is written, so those migrations are authored against the 0.2.0 contract.
---

# 046: rahi 0.2.0 adoption

## 1. Purpose

Constitution VI makes the chassis a versioned dependency, never a fork, and
spec 010 pins it exactly so that a chassis bump is a single, reviewable
change. rahi `v0.2.0` (released 2026-09-20, `rahi://039`) is that change.
Its CHANGELOG classifies it as a minor bump because key sets, backup
authentication, recovery, and the export API change compatibility. The
parts this repository touches today:

- **Migrations (`rahi://036`).** `rahi_store::Migration` gains an
  `additive` flag and a `Migration::additive()` builder. A migration that
  does not declare itself additive is not additive. `serve` accepts a store
  ahead of the binary only when every applied version above the binary's
  last is additive. `schema_version` gains `checksum` and `additive`
  columns, and a recorded version whose SQL differs from the binary's is
  `Error::Integrity` (exit 1). `Migration::new(version, name, sql)` is
  unchanged, so `crates/aicortex-store/src/migrations.rs` compiles as is;
  what changes is what an undeclared migration means.
- **Manifest transitions (`rahi://036`).** `rahi migrate --adopt-manifest`
  appends a `manifest.transition` decision. The boot check compares against
  the chain's current manifest, not the first-booted one.
- **Recovery (`rahi://037`).** Key sets carry `backup_passkey.json`;
  `first-boot --export` requires `RAHI_PUBLIC_URL`;
  `rahi_ops::first_boot::export` and `rahi_ops::restore::run` change
  signature. A pre-0.2.0 key set cannot take a backup until a passkey is
  provisioned.
- **Ledger identity (`rahi://042`).** Every decision carries a resident
  identity row that survives sealing, `Ledger::append` is lifetime
  idempotent, and `Ledger::lookup` distinguishes five answers. Spec 013's
  refusal and quarantine Decisions and spec 014's erasure Decisions inherit
  this without code change here.

The `Cell` trait, manifest schema `1.0.0`, the exit-code mapping, the
archive envelope, and the chain record format of existing records do not
change.

Dependabot has opened per-crate branches (`rahi-ops`, `rahi-store`) at
0.2.0. Those are not adoptable one at a time: spec 010 pins one version for
all nine crates, and rahi releases the nine together.

## 2. Territory

No new file. The nine `rahi-*` lines of the workspace manifest's
`[workspace.dependencies]` section (spec 010), `deny.toml` for the revisit
spec 010 D-6 and D-8 require at every chassis bump, the migration list of
spec 012 for the `additive` declarations, and the two binary tests that
exercise boot and migrate (`apps/aicortex/tests/cell.rs`,
`apps/aicortex/tests/migrate.rs`). `Cargo.lock` moves with the manifest.

## 3. Behavior

- **B-1 (one version, nine crates).** All nine chassis crates move from
  `=0.1.0` to `=0.2.0` in one commit. The exact-pin form is kept (spec 010).
  A tree in which any two `rahi-*` crates resolve to different versions is
  refused by the test of FR-001.
- **B-2 (additive is declared, never assumed).** Every migration this
  repository ships is reviewed and either declared with
  `Migration::additive()` or left undeclared, and the choice is recorded per
  version in section 7 by the build session. A migration that only creates a
  new table or index, and whose absence an older binary tolerates, is a
  candidate for `additive`; a migration that rewrites or re-digests rows (for
  example the fingerprint re-digest spec 014 carries) is not. D-2 records
  the review of versions 1 to 3.
- **B-3 (the checksum is the shipped SQL).** No shipped migration's SQL is
  edited to adopt 0.2.0. rahi 0.2.0 records the binary's checksum on the
  first `migrate` for a row written before it, and refuses a later mismatch
  as `Error::Integrity`; editing shipped SQL would turn every existing store
  into an integrity failure. This restates spec 012 B-1 against the new
  chassis behavior.
- **B-4 (manifest adoption is part of migrate).** The operator path for
  this cell runs `migrate --adopt-manifest`, so that a manifest change after
  this release is a chain record rather than a boot failure. The packaging
  spec (043) owns the entrypoint; this spec records the requirement and
  names 043 as where it lands.
- **B-5 (the supply-chain revisit runs again).** Spec 010 D-6 and D-8
  require every advisory ignore, licence exception, and duplicate-version
  waiver to be revisited at a chassis bump. The build session runs `cargo
  deny check` against the 0.2.0 tree, removes any waiver the new tree no
  longer needs, and records each one that stays with its reason. Nothing is
  relaxed to get a green run.

## 4. Functional requirements

- **FR-001.** A test asserts that every `rahi-*` package in `cargo metadata
  --locked` resolves to `0.2.0`.
- **FR-002.** `aicortex migrate` on an empty data directory applies every
  migration and records a checksum and the declared `additive` flag for
  each; a second run is a no-op.
- **FR-003.** `aicortex serve` against a store one additive migration ahead
  of the binary starts; against a store one non-additive migration ahead it
  exits 2 naming that version.
- **FR-004.** `tests/cell.rs` still asserts the manifest's `schema_version`
  against `rahi_types::MANIFEST_SCHEMA_VERSION` (spec 010 D-7) and passes
  unchanged in intent.

## 5. Acceptance criteria

- **AC-1.** `cargo test --workspace --locked` passes on the 0.2.0 tree.
- **AC-2.** `cargo deny check` passes, with every remaining waiver
  recorded against the 0.2.0 tree.
- **AC-3.** FR-002 and FR-003 hold through the built binary.

## 6. Out of scope

The container entrypoint and Kubernetes job that pass `--adopt-manifest`
(043). Backup passkey provisioning for an existing deployment, which rahi
0.2.0 states it does not automate; no aicortex deployment exists yet, so
there is no key set to migrate. Any use of the new ledger lookup API, which
spec 024 may adopt when it is built.

## 7. Resolved decisions

- **D-1 (2026-09-24, owner direction).** The rahi pins move from `=0.1.0`
  to 0.2.0. This spec keeps spec 010's exact-pin rule and writes `=0.2.0`;
  a caret requirement would let a later 0.2.x patch arrive without review,
  which spec 010 refuses.

- **D-2 (2026-09-24, owner decision).** Recorded from the owner's answer
  to this draft's questions: "I agree with all your suggestions and
  recommendations." The owner chose the conservative rule for B-2: declare
  an existing migration additive only if reading it verifies it. This
  session read all three against rahi 0.2.0's definition (a migration that
  "only creates tables, indexes, or nullable or defaulted columns", rahi
  spec 036 B-8), and each qualifies:
  - version 1, `rahi_store::coordination_migration`: `CREATE TABLE IF NOT
    EXISTS` for `lease_fence` and `outbox`, nothing else;
  - version 2, "aicortex memory schema": `CREATE TABLE IF NOT EXISTS` for
    `scope`, `memory`, `provenance`, `memory_derivation`, and
    `scope_counter`, and `CREATE [UNIQUE] INDEX IF NOT EXISTS` for three
    indexes, with no `UPDATE`, `DELETE`, `INSERT`, or `ALTER`;
  - version 3, "aicortex decision digest keys": `CREATE TABLE IF NOT
    EXISTS decision_key` and one `CREATE INDEX IF NOT EXISTS`.
  The build session declares all three with `.additive()`. The declaration
  does not change a migration's SQL, so rahi's checksum (over the SQL) is
  unchanged and B-3 holds. The re-digest migration spec 014 carries is not
  additive and must not be declared so.

## 8. Open questions

- **Q-2.** Should the per-crate dependabot branches be closed in favor of
  this change, and should `.github/dependabot.yml` group the nine `rahi-*`
  crates so the next bump arrives as one pull request? The dependabot file
  is spec 001's territory.

## 9. Obligations

Declared here in the spec-spine 106 grammar and to be lifted into the
frontmatter `obligations` key when this repository's spec-spine pin moves
to 0.25.0, which is a separate follow-up (below it, the key is a compile
error, `V-002`).

```yaml
obligations:
  - { id: "I-1", kind: invariant, text: "All nine rahi crates resolve to one exact version.", anchor: "3-behavior" }
  - { id: "I-2", kind: invariant, text: "No shipped migration's SQL is edited; a correction is a new migration.", anchor: "3-behavior" }
  - { id: "R-1", kind: requirement, text: "Every shipped migration's additive status is declared and recorded per version.", anchor: "3-behavior" }
```

## Verification

```verify:cli
# planned: these commands pass once the pin moves; before that the first fails by design.
cargo tree --workspace --locked -i rahi-store@0.2.0
cargo test --workspace --locked
cargo test -p aicortex --locked --test migrate
```
