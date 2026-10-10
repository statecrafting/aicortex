---
id: "058-rahi-0-6-0-adoption"
title: "Move the chassis to rahi 0.6.0 and action-gate-core to 0.3.0, with closed mode left off"
status: approved
kind: "tooling"
domain: "chassis"
created: "2026-10-10"
authors: ["Bartek Kus"]
implementation: in-progress
risk: high
wave: 4
depends_on:
  - "046-rahi-0-2-0-adoption"
  - "047-shared-secret-detector-convergence"
extends:
  - { spec: "010-chassis-adoption-and-workspace", unit: "Cargo.toml", nature: additive }
  - { spec: "010-chassis-adoption-and-workspace", unit: "deny.toml", nature: additive }
  - { spec: "010-chassis-adoption-and-workspace", unit: "apps/aicortex/tests/cell.rs", nature: additive }
references:
  - { unit: { kind: file, path: "specs/010-chassis-adoption-and-workspace/spec.md" }, role: constraint }
  - { unit: { kind: file, path: "specs/046-rahi-0-2-0-adoption/spec.md" }, role: context }
  - { unit: { kind: file, path: "specs/047-shared-secret-detector-convergence/spec.md" }, role: context }
  - { unit: { kind: file, path: "docs/design/02-rahi-chassis-prerequisites-handoff.md" }, role: context }
obligations:
  - { id: "I-1", kind: invariant, text: "All rahi crates in the resolved tree resolve to one exact version, 0.6.0.", anchor: "3-behavior" }
  - { id: "I-2", kind: invariant, text: "No gate built by this workspace runs in action-gate's closed mode unless a spec requires it.", anchor: "3-behavior" }
  - { id: "R-1", kind: requirement, text: "Every duplicate the new chassis tree forces into a refused stack is waived by exact version with its reason.", anchor: "3-behavior" }
summary: >
  The nine rahi crates are pinned at `=0.4.0` and action-gate-core at
  `=0.2.0`. rahi 0.6.0 (crates.io) adds managed services (rahi 047),
  cell-contributed preflight checks (rahi 049), and the binding document
  (rahi 040), all through default `Cell` methods, so a cell that overrides
  none of them keeps its lifecycle. action-gate-core 0.3.0 adds an opt-in
  closed mode and leaves every open gate's evaluation and hash unchanged.
  This spec moves both pins in one change, keeps the exact-pin rule, leaves
  closed mode off, re-runs the supply-chain revisit spec 010 requires at a
  chassis bump, and refreshes the chassis handoff note. It is the
  prerequisite for finishing spec 015, whose worker lifecycle and product
  preflight wait on rahi 047 and 049.
---

# 058: rahi 0.6.0 and action-gate-core 0.3.0 adoption

## 1. Purpose

Constitution VI makes the chassis a versioned dependency, never a fork, and
spec 010 pins it exactly so that a chassis bump is one reviewable change.
Spec 046 set the shape: all nine crates move together, the exact pin is
kept, and `deny.toml` is revisited against the new tree.

rahi `v0.6.0` (`rahi://039`, CHANGELOG "0.6.0") is the release spec 015's
Status section waits for. Its consumer-contract changes since 0.4.0:

- **0.5.0.** `rahi-ops` fencing functions changed (`create_fences`), and
  `serve` and the verbs change their exit behavior on a signal (rahi 048).
  This workspace calls neither fencing function; it calls `rahi_cli::run`.
- **0.6.0.** `Cell` gains three default methods: `services` (rahi 047,
  managed background services joined through the lifecycle),
  `preflight_checks` (rahi 049, named `AppCheck`s run after the chassis's
  own checks), and `app_revision` (rahi 040, the binding document).
  `rahi_cli::serve::Composed` gains a public field and
  `rahi_ops::stop::Reason` gains variants. `/readyz` holds 503 for a
  readiness window during a stop (rahi 043 B-9). A cell that overrides none
  of the new methods needs no code change.

action-gate-core `0.3.0` (action-gate spec 004) is additive: `Mode::Closed`
is opt-in through `GateBuilder::closed()` or `require`, and a 0.2.0 gate
evaluates and hashes exactly as before. aicortex-gate consumes only
`action_gate_core::secrets`, the detector registry and its golden vectors
(spec 047), which are unchanged.

## 2. Territory

No new file. The nine `rahi-*` lines and the action-gate-core line of the
workspace manifest (spec 010), `deny.toml` for the revisit spec 010 D-6 and
D-8 require at every chassis bump, the version constant of the binary test
that asserts the resolved chassis version (`apps/aicortex/tests/cell.rs`,
spec 046 FR-001), and `docs/design/02-rahi-chassis-prerequisites-handoff.md`.
`Cargo.lock` moves with the manifest.

## 3. Behavior

- **B-1 (one version, nine crates).** All nine chassis crates move from
  `=0.4.0` to `=0.6.0` in one commit. The exact-pin form is kept (spec 010,
  046 D-1). Spec 046 FR-001's test asserts the version every resolved
  `rahi-*` package carries, now `0.6.0`.
- **B-2 (action-gate-core moves with it).** action-gate-core moves from
  `=0.2.0` to `=0.3.0`, default features only, as spec 047 B-4 pins it.
  `action-gate-types` stays at 0.1, which both versions share.
- **B-3 (closed mode stays off).** No gate this workspace builds calls
  `GateBuilder::closed()`, `require`, or `require_all`. Adopting closed mode
  is a behavior change to admission and belongs to a spec that requires it
  (draft spec 057 proposes exactly that). Until such a spec is approved and
  built, every gate here is an open gate with the 0.2.0 evaluation and
  config hash.
- **B-4 (no new Cell method is overridden here).** This change keeps the
  cell's lifecycle as it was. Mounting the embedding worker as a managed
  service and contributing the embedding preflight check belong to spec
  015, which owns that behavior; this spec only makes the hooks available.
- **B-5 (the supply-chain revisit runs again).** `cargo deny check` runs
  against the 0.6.0 tree. A duplicate the chassis itself introduces into a
  stack `deny.toml` refuses to see twice is something this workspace cannot
  converge (spec 010 D-6). It is waived by exact version, never by crate
  name, with the reason and the crate that pulls each copy, so the waiver
  lapses by itself when rahi converges. Nothing else is relaxed.
- **B-6 (the handoff note describes today).** The chassis handoff note
  stops describing 2026-09-19. It records that its three prerequisites were
  met on rahi 0.4.0 (spec 014's completion), and what this repository now
  consumes from or still needs of the chassis.

## 4. Functional requirements

- **FR-001.** `fr001_046_every_rahi_package_resolves_to_one_version`
  asserts `0.6.0` for every resolved `rahi-*` package.
- **FR-002.** `cargo tree --locked -i action-gate-core@0.3.0` shows
  aicortex-gate as its consumer.
- **FR-003.** No source file in the workspace calls `closed()`,
  `require(`, or `require_all(` on an action-gate builder.
- **FR-004.** Spec 047's golden-vector parity test passes unchanged against
  0.3.0.

## 5. Acceptance criteria

- **AC-1.** `cargo test --workspace --locked` passes on the 0.6.0 tree.
- **AC-2.** `cargo deny check` passes, with every waiver this change adds
  recorded against the 0.6.0 tree in section 7.

## 6. Out of scope

Overriding `Cell::services`, `Cell::preflight_checks`, or
`Cell::app_revision` (015 and later specs). Adopting action-gate's closed
mode (057). Converging the duplicate copies the chassis carries, which is
rahi's work.

## 7. Resolved decisions

- **D-1 (2026-10-10, owner work order).** The owner directed this change:
  move rahi to `=0.6.0` and action-gate-core to `=0.3.0`, leave closed mode
  off unless a spec requires it, and refresh the handoff note. That
  direction is the approval of this spec. Exact pins, per 046 D-1.

- **D-2 (2026-10-10, closed mode left off).** B-3. Closed mode changes
  what admission decides when no check decides, which spec 013 and 051 own;
  turning it on as a side effect of a version bump would change admission
  without the differential evidence draft spec 057 proposes. Draft 057 also
  moves the action-gate-core pin; once this spec lands, 057's pin move is
  already done and only its adoption of closed mode remains.

- **D-3 (2026-10-10, build record for B-5).** The resolved 0.6.0 tree
  carries two copies of two crates, both forced by rahi 0.6.0:
  - `tower-http` 0.6.11 (through `reqwest` 0.13.5 under `cryptr` and
    `hiqlite-patched`) and 0.7.1 (through `rahi-edge`, which rahi's own
    manifest takes "a minor ahead of the copy reqwest carries"). `deny.toml`
    refuses two `tower-http` copies, so 0.6.11 is skipped by exact version
    with that reason. The refusal stays in force for every other version.
  - `action-gate-core` 0.2.0 (through `rahi-kernel` 0.6.0, which still pins
    `=0.2.0`) and 0.3.0 (this workspace). It is outside every refused stack,
    so it is reported as a warning only, and no action-gate type crosses
    between the two: no aicortex crate calls a `rahi-kernel` item that
    returns or takes an action-gate `Gate`. The copy disappears when a rahi
    release moves `rahi-kernel` to 0.3.0.
  `opentelemetry` moves 0.32 to 0.33 as one family under rahi-edge, with no
  duplicate. The workspace declares `rahi-harness` at `=0.6.0`, and no
  member depends on it, as 046 D-4 recorded for 0.2.0.

## 8. Open questions

None.

## Verification

```verify:cli
cargo tree --workspace --locked -i rahi-store@0.6.0
cargo tree --workspace --locked -i action-gate-core@0.3.0
cargo test -p aicortex --locked --test cell
cargo test -p aicortex-gate --locked
```
