---
id: "055-release-and-crate-publication"
title: "Release the library crates: a tag publishes, a human cuts the tag, and nothing else is implied"
status: approved
kind: "tooling"
domain: "ops"
created: "2026-09-27"
authors: ["Bartek Kus"]
implementation: pending
risk: high
wave: 5
depends_on:
  - "053-host-library-mode"
establishes:
  - ".github/workflows/release.yml"
  - "scripts/release-check.sh"
  - "docs/release.md"
  - "CHANGELOG.md"
extends:
  - { spec: "011-memory-model", unit: "crates/aicortex-types/Cargo.toml", nature: additive }
  - { spec: "012-store-schema-and-repositories", unit: "crates/aicortex-store/Cargo.toml", nature: additive }
  - { spec: "013-write-gate-and-redaction", unit: "crates/aicortex-gate/Cargo.toml", nature: additive }
  - { spec: "050-typed-claims-and-predicate-registry", unit: "crates/aicortex-claims/Cargo.toml", nature: additive }
  - { spec: "010-chassis-adoption-and-workspace", unit: { kind: section, file: "Cargo.toml", anchor: "workspace.package" }, nature: additive }
references:
  - { unit: { kind: file, path: "specs/053-host-library-mode/spec.md" }, role: constraint }
obligations:
  - { id: "I-1", kind: invariant, text: "Only an annotated tag on a commit already on main, pushed by a human, publishes; no workflow, schedule, or agent creates the tag.", anchor: "3-1-what-publishes-and-who-decides" }
  - { id: "I-2", kind: invariant, text: "Only the four library crates named in B-2 are publishable; the app and every fixture keep publish = false.", anchor: "3-1-what-publishes-and-who-decides" }
  - { id: "I-3", kind: invariant, text: "A release note distinguishes published from deployed, qualified, and production-ready, and asserts none of the latter three.", anchor: "3-3-what-a-release-says" }
  - { id: "R-1", kind: requirement, text: "A registry consumer outside this workspace builds against the published crates with no path, git, or patch source and runs the 053 external-host proof.", anchor: "3-2-the-release-check" }
summary: >
  No aicortex crate has been published, so host-library consumers (053) pin
  an exact source revision. This spec makes the four library crates that
  host-library mode needs (types, store, gate, claims) publishable to
  crates.io under one lockstep version, adds a release check that packages
  them and builds a registry-only consumer, and adds a workflow that
  publishes only from an annotated tag a human pushed. Publication stays a
  separate human act from merge, deployment, and qualification, and a
  release note must say which of those it is.
---

# 055: Release and crate publication

## 1. Purpose

The backlog records the state plainly: host-library mode "is available to
tm at the exact source revision", "no Aicortex crate has been published, no
Aicortex release has been created", and AIC-REL defers "whether to publish
Aicortex crates, create a signed release, qualify external consumers, or
deploy an exact image" until an owner selects a completed product slice
(`docs/backlog.md`). Spec 053 is that slice for library consumers: its
contract is exactly what a linked host consumes.

A source-revision pin is the consumption pattern this family already
learned is fragile. Spec 010 D-1 refuses git and path dependencies on the
chassis for the reason that a `[patch]` or a git revision is not what a
registry consumer executes, and the 014 chassis handoff
(`docs/design/02-rahi-chassis-prerequisites-handoff.md` section 2) records
the cost when publication lagged the code. aicortex's consumers deserve the
same property it demands of rahi: an exact published version whose bytes
are the bytes that were tested. This spec supplies the mechanism and keeps
the decision to use it with a human, as rahi 039 does for the chassis.

## 2. Territory

A release workflow (`.github/workflows/release.yml`), a local and CI
release check (`scripts/release-check.sh`), the release procedure
(`docs/release.md`), and a changelog (`CHANGELOG.md`). It extends the four
library crate manifests (publish flag and crates.io metadata) and the
workspace `[workspace.package]` section (one shared version). It does not
touch the app, the image (043), or any deployment (044).

## 3. Behavior

### 3.1 What publishes and who decides

- **B-1 (tag is the trigger).** The release workflow MUST run only on the
  push of an annotated tag matching `v<semver>` whose commit is reachable
  from `main`. It MUST refuse a lightweight tag, a tag on a commit not on
  `main`, and a tag whose version differs from `[workspace.package]
  version`. No workflow, schedule, or agent session creates or pushes the
  tag; that is the human act (I-1).
- **B-2 (the publishable set).** Exactly `aicortex-types`,
  `aicortex-store`, `aicortex-gate`, and `aicortex-claims` become
  publishable, in that dependency order. `apps/aicortex` and every crate
  under `fixtures/` keep `publish = false`. A crate added later is
  publishable only through an amendment naming it.
- **B-3 (lockstep version).** The four crates share one version inherited
  from `[workspace.package] version`, and their inter-crate requirements
  stay exact (`=x.y.z`), matching how this workspace pins rahi (010 B-2).
  Before 1.0.0 any release MAY break the API; the changelog says whether it
  did.
- **B-4 (irreversibility).** A crates.io version can be neither
  unpublished nor reused. The workflow MUST publish in dependency order,
  stop at the first failure, and report which crates published, so a
  partial release is recovered by publishing the rest at the same version
  or by yanking and releasing the next version, never by retagging.

### 3.2 The release check

- **B-5 (package before publish).** `scripts/release-check.sh` MUST run
  `cargo package --locked` for each B-2 crate, confirm every file a crate
  reads at compile time (migration SQL, `include_str!` inputs, test-free
  fixtures it embeds) is inside its package, and refuse a package that
  would carry a path to `fixtures/`, `data/`, or `.derived/`.
- **B-6 (registry-only consumer).** The check MUST build a throwaway
  consumer outside the workspace that depends on the packaged crates
  through a local registry overlay only, with no path, git, or `[patch]`
  source, and run the external-host atomicity proof of 053 (R-1 there)
  against them. This is the evidence that a registry consumer gets what CI
  tested, which a green workspace run does not show.
- **B-7 (runs on every PR).** The check without the publish step MUST run
  on every pull request that touches a B-2 crate, so a crate that stops
  packaging is found at merge time, not at release time.

### 3.3 What a release says

- **B-8 (release note).** Each release MUST add a `CHANGELOG.md` section
  naming the version, the exact commit, the rahi version pinned, the specs
  whose implementation it carries, breaking changes, and known
  limitations. It MUST state that the release is a publication of library
  crates and that it asserts no deployment, image, reference-deployment
  qualification, or production readiness (I-3).
- **B-9 (no secret in CI).** The registry token is a repository secret
  scoped to the release environment, which requires a human approval in
  the GitHub environment protection rules. The workflow MUST NOT expose it
  to pull-request runs.

## 4. Functional requirements

- **FR-001.** `scripts/release-check.sh` exits 0 on the tree and non-zero
  when a B-2 crate omits a compile-time input from its package, asserted by
  a fixture that removes one migration file from a copy.
- **FR-002.** The registry-only consumer of B-6 builds with `--locked` and
  its atomicity test passes against the packaged crates.
- **FR-003.** The workflow's tag guard refuses a lightweight tag, a
  non-`main` commit, and a version mismatch, each asserted by a dry-run
  mode of the workflow's guard script against fixture refs.
- **FR-004.** `apps/aicortex` and every fixture crate report
  `publish = false`; the four B-2 crates report a publishable manifest with
  `description`, `license`, `repository`, and `readme` set.

## 5. Acceptance criteria

- **AC-1.** `scripts/release-check.sh` exits 0.
- **AC-2.** `scripts/release-check.sh --self-test` exits 0, exercising
  FR-001 and FR-003's negative cases.
- **AC-3.** The first real publication is a human act recorded in
  `CHANGELOG.md` with the crates.io versions and the tag's commit; this
  spec is `implementation: complete` when the mechanism and AC-1 and AC-2
  land, whether or not a release has been cut.

## 6. Out of scope

Publishing `aicortex-embed`, `-index`, `-graph`, `-recall`, `-api`, `-mcp`,
or any later crate before its spec is complete and an amendment names it.
Container images and their signing (043). Deployment and qualification
(044). Choosing when to cut the first release, which is the owner's
AIC-REL decision.

## 7. Resolved decisions

None yet. Open for the owner before approval:

- **Q-1.** Whether `aicortex-gate` belongs in the first publishable set.
  053's host path uses store and claims; the gate is needed if a host
  admits content through aicortex's write gate, which 053 does not require.
- **Q-2.** Whether the first version is `0.1.0` (the current manifest
  version) or a version that signals the 053 contract, for example
  `0.2.0`.

## Verification

```verify:cli
scripts/release-check.sh
scripts/release-check.sh --self-test
```
