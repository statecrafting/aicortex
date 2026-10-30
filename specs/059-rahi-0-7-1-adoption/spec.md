---
id: "059-rahi-0-7-1-adoption"
title: "Move every chassis pin to rahi 0.7.1 together, and drop the second action-gate-core"
status: draft
kind: "tooling"
domain: "chassis"
created: "2026-10-29"
authors: ["Bartek Kus"]
implementation: pending
risk: high
wave: 4
depends_on:
  - "058-rahi-0-6-0-adoption"
extends:
  - { spec: "010-chassis-adoption-and-workspace", unit: "Cargo.toml", nature: additive }
  - { spec: "010-chassis-adoption-and-workspace", unit: "deny.toml", nature: additive }
  - { spec: "010-chassis-adoption-and-workspace", unit: "apps/aicortex/tests/cell.rs", nature: additive }
references:
  - { unit: { kind: file, path: "specs/010-chassis-adoption-and-workspace/spec.md" }, role: constraint }
  - { unit: { kind: file, path: "specs/046-rahi-0-2-0-adoption/spec.md" }, role: context }
  - { unit: { kind: file, path: "specs/058-rahi-0-6-0-adoption/spec.md" }, role: context }
obligations:
  - { id: "I-1", kind: invariant, text: "All rahi crates in the resolved tree resolve to one exact version, 0.7.1.", anchor: "3-behavior" }
  - { id: "I-2", kind: invariant, text: "One copy of action-gate-core resolves, at 0.3.0.", anchor: "3-behavior" }
  - { id: "R-1", kind: requirement, text: "Every chassis-only waiver in deny.toml is re-checked against the 0.7.1 tree and removed when it no longer matches.", anchor: "3-behavior" }
summary: >
  The nine rahi crates are pinned at `=0.6.0` (spec 058). rahi 0.7.1
  (crates.io) is the patch on 0.7.0, which adds deployment epochs, remote
  identity, and the split N=3 overlay, all behind items and environment
  variables this cell does not use, so a cell that calls only
  `rahi_cli::run` needs no code change. 0.7.0 also moves `rahi-kernel` to
  action-gate-core `=0.3.0`, which removes the second action-gate-core copy
  spec 058 D-3 recorded. This spec moves all nine pins in one change, keeps
  the exact-pin rule, re-runs the supply-chain revisit spec 010 requires at
  a chassis bump, and replaces the partial bump dependabot proposed in #72,
  which moved seven of the nine crates and broke spec 046's one-version
  invariant on main.
---

# 059: rahi 0.7.1 adoption

## 1. Purpose

Constitution VI makes the chassis a versioned dependency, never a fork, and
spec 010 pins it exactly so that a chassis bump is one reviewable change.
Spec 046 set the shape and spec 058 repeated it: all nine crates move
together, the exact pin is kept, and `deny.toml` is revisited against the
new tree.

Dependabot's #72 moved seven crates to `=0.7.1` and left `rahi-idp` and
`rahi-harness` at `=0.6.0`, which spec 046 FR-001's test refuses; PR #85
returned all nine to `=0.6.0`. This spec is the governed path forward.

rahi `v0.7.1` (`rahi://039`, CHANGELOG "0.7.0" and "0.7.1") changes the
consumer contract since 0.6.0 as follows:

- **0.7.0.** `rahi_ledger` gains the `epoch` module and
  `Ledger::current_epoch`; `rahi_kernel::KernelOptions` gains a `binding`
  field; the archive manifest gains an optional `epoch`; `rahi_idp` gains
  `back_channel`; `rahi_edge` gains `router::switching` and `/startupz`.
  `RAHI_RAUTHY_MODE=remote` is opt-in. `rahi-kernel` takes
  action-gate-core `=0.3.0` and attest-ledger primitives 0.2, with record
  envelopes and hashes unchanged. The N=1 supervised composition is
  unchanged and stays the only qualified topology.
- **0.7.1.** A remote cell provisions the backup admin. Lockfile-only
  dependency moves. No public item, environment variable, exit code,
  archive or record format moves.

No workspace source names `KernelOptions`, `rahi_cli::serve::Composed`,
`rahi_ops::stop::Reason`, or the archive manifest, so the added field and
variants reach nothing here.

## 2. Territory

No new file. The nine `rahi-*` lines of the workspace manifest (spec 010),
`deny.toml` for the revisit spec 010 D-6 and D-8 require at every chassis
bump, and the binary test that asserts the resolved chassis version
(`apps/aicortex/tests/cell.rs`, spec 046 FR-001): its version constant
moves, and it gains the FR-003 assertion on action-gate-core.
`Cargo.lock` moves with the manifest.

## 3. Behavior

- **B-1 (one version, nine crates).** All nine chassis crates move from
  `=0.6.0` to `=0.7.1` in one commit, `rahi-idp` and `rahi-harness`
  included. The exact-pin form is kept (spec 010, 046 D-1). Spec 046
  FR-001's test asserts `0.7.1` for every resolved `rahi-*` package.
- **B-2 (one action-gate-core).** With `rahi-kernel` on action-gate-core
  `=0.3.0`, the resolved tree carries one copy, the one this workspace
  already pins (spec 058 B-2). Closed mode stays off, as spec 058 B-3
  requires.
- **B-3 (no new chassis behavior is turned on).** This cell sets no
  `RAHI_RAUTHY_MODE`, adopts no epoch, and overrides no new `Cell`
  method. Adopting any of them is a behavior change that belongs to the
  spec that owns it (043 for operations, 044 for the reference
  deployment).
- **B-4 (the supply-chain revisit runs again).** `cargo deny check` runs
  against the 0.7.1 tree. Each chassis-only advisory ignore and exact
  version skip in `deny.toml` is checked against the new tree: one that no
  longer matches anything is removed, and one that still matches keeps its
  reason, updated to name 0.7.1. Nothing is relaxed.

## 4. Functional requirements

- **FR-001.** `fr001_046_every_rahi_package_resolves_to_one_version`
  asserts `0.7.1` for every resolved `rahi-*` package.
- **FR-002.** `b2_the_workspace_pins_nine_rahi_crates_to_one_exact_version`
  passes with all nine at `=0.7.1`.
- **FR-003.** A test in `apps/aicortex/tests/cell.rs` reads `cargo
  metadata --locked` and asserts that `action-gate-core` resolves exactly
  once, at `0.3.0`, so a second copy fails `cargo test` rather than
  passing unnoticed. `cargo tree --locked -i action-gate-core@0.2.0`
  finds no package.
- **FR-004.** Spec 047's golden-vector parity test passes unchanged.

## 5. Acceptance criteria

- **AC-1.** `cargo test --workspace --locked` passes on the 0.7.1 tree.
- **AC-2.** `cargo deny check` passes, and section 7 records which
  chassis-only waivers the 0.7.1 tree still needs.

## 6. Out of scope

Remote identity, deployment epochs, and the split N=3 overlay (043, 044).
Converging the chassis's own duplicate copies or moving it off `age 0.12`,
which is rahi's work; the two advisories `deny.toml` ignores for `age`
(spec 010 D-6) stay until a rahi release does. Changing dependabot's
grouping.

## 7. Resolved decisions

- **D-1 (2026-10-29, owner work order).** The owner directed this change:
  write the rahi 0.7.1 adoption as a governed spec instead of merging the
  partial dependabot bump. Approval of this draft remains a human flip.

- **D-2 (2026-10-29, pre-build trial, recorded for the build).** A trial
  resolution on the tree PR #85 leaves (all nine pins moved to `=0.7.1`,
  `cargo update` on the eight resolved chassis crates) removed
  action-gate-core 0.2.0 and moved attest-ledger-core and
  attest-ledger-types 0.1.0 to 0.2.0, with no other chassis crate added.
  `cargo deny check` passed: advisories, bans, licences and sources ok,
  31 duplicate warnings, and no waiver added. `age` stays at 0.12.1 under
  `rahi-ops`, so RUSTSEC-2026-0374 and RUSTSEC-2026-0387 still match. The
  trial did not compile or test the workspace. AC-1 and the waiver
  re-check in B-4 are the build's to record.

## 8. Open questions

None.

## Verification

```verify:cli
cargo tree --workspace --locked -i rahi-store@0.7.1
cargo tree --workspace --locked -i action-gate-core@0.3.0
cargo test -p aicortex --locked --test cell
cargo test -p aicortex-gate --locked
cargo deny check
```
