---
id: "055-release-and-crate-publication"
title: "Release the library crates: a tag publishes, a human cuts the tag, and nothing else is implied"
status: approved
kind: "tooling"
domain: "ops"
created: "2026-09-27"
authors: ["Bartek Kus"]
implementation: in-progress
risk: high
wave: 5
depends_on:
  - "053-host-library-mode"
establishes:
  - ".github/workflows/release.yml"
  - "scripts/release-check.sh"
  # Markdown is in no content hash, so each document is claimed through its
  # top-level section, which hashes the whole file through its span.
  - { kind: section, file: "docs/release.md", anchor: "releasing-the-library-crates" }
  - { kind: section, file: "CHANGELOG.md", anchor: "changelog" }
  - { kind: section, file: "crates/aicortex-types/README.md", anchor: "aicortex-types" }
  - { kind: section, file: "crates/aicortex-claims/README.md", anchor: "aicortex-claims" }
  - { kind: section, file: "crates/aicortex-gate/README.md", anchor: "aicortex-gate" }
  - { kind: section, file: "crates/aicortex-store/README.md", anchor: "aicortex-store" }
  - { kind: section, file: "crates/aicortex-embed/README.md", anchor: "aicortex-embed" }
extends:
  - { spec: "011-memory-model", unit: "crates/aicortex-types/Cargo.toml", nature: additive }
  - { spec: "012-store-schema-and-repositories", unit: "crates/aicortex-store/Cargo.toml", nature: additive }
  - { spec: "013-write-gate-and-redaction", unit: "crates/aicortex-gate/Cargo.toml", nature: additive }
  - { spec: "050-typed-claims-and-predicate-registry", unit: "crates/aicortex-claims/Cargo.toml", nature: additive }
  - { spec: "015-embedding-pipeline", unit: "crates/aicortex-embed/Cargo.toml", nature: additive }
  - { spec: "010-chassis-adoption-and-workspace", unit: "deny.toml", nature: additive }
  - { spec: "010-chassis-adoption-and-workspace", unit: { kind: section, file: "Cargo.toml", anchor: "workspace.package" }, nature: additive }
references:
  - { unit: { kind: file, path: "specs/053-host-library-mode/spec.md" }, role: constraint }
obligations:
  - { id: "I-1", kind: invariant, text: "Only an annotated tag on a commit already on main, pushed by a human, publishes; no workflow, schedule, or agent creates the tag.", anchor: "3-1-what-publishes-and-who-decides" }
  - { id: "I-2", kind: invariant, text: "Only the five library crates named in B-2 are publishable; the app and every fixture keep publish = false.", anchor: "3-1-what-publishes-and-who-decides" }
  - { id: "I-3", kind: invariant, text: "A release note distinguishes published from deployed, qualified, and production-ready, and asserts none of the latter three.", anchor: "3-3-what-a-release-says" }
  - { id: "R-1", kind: requirement, text: "A registry consumer outside this workspace builds against the published crates with no path, git, or patch source and runs the 053 external-host proof.", anchor: "3-2-the-release-check" }
summary: >
  No aicortex crate has been published, so host-library consumers (053) pin
  an exact source revision. This spec makes the five library crates that
  host-library mode needs (types, claims, gate, store, embed) publishable to
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
(`docs/release.md`), a changelog (`CHANGELOG.md`), and a readme for each
publishable crate. It extends the five library crate manifests (publish
flag and crates.io metadata), the workspace `[workspace.package]` section
(one shared version), and `deny.toml` (D-4). It does not
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
  `aicortex-claims`, `aicortex-gate`, `aicortex-store`, and
  `aicortex-embed` become publishable, in that dependency order (amended
  2026-11-01, D-1). `apps/aicortex` and every crate
  under `fixtures/` keep `publish = false`. A crate added later is
  publishable only through an amendment naming it.
- **B-3 (lockstep version).** The five crates share one version inherited
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
  would carry a path to `fixtures/`, `data/`, or `.statecraft/derived/`.
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
  `publish = false`; the five B-2 crates report a publishable manifest with
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

- **D-1 (2026-11-01, owner-approved amendment of B-2 and I-2).**
  `aicortex-embed` joins the publishable set, which becomes types, claims,
  gate, store, embed in dependency order. B-6 requires a registry-only
  consumer to run the 053 external host proof, and that proof activates an
  embedding model through `aicortex_embed::ModelRegistry` because a memory
  write stages embedding work (spec 015, after 053 landed); with four
  crates no registry consumer could run it. Spec 015 is complete and every
  dependency of the crate resolves from crates.io. The order in the
  original B-2 (types, store, gate, claims) also contradicted the
  manifests, since the store depends on the gate and the claims; the
  amended order is the one the manifests impose. Rejected: packaging the
  embed crate into the overlay without publishing it, which would prove
  what no real consumer can reproduce.
- **D-2 (2026-11-01, resolves Q-1).** `aicortex-gate` stays in the set. It
  is a normal dependency of `aicortex-store` (`MemoryRepo::insert` takes the
  gate's `Admitted`), so the store cannot publish without it, whether or not
  a host admits content through the gate itself.
- **D-3 (2026-11-01, Q-2 left to the release).** The mechanism keeps the
  manifest version `0.1.0` and is version-agnostic: the release pull
  request sets `[workspace.package] version` and the `=x.y.z` workspace
  entries, and the owner chooses the number then (AIC-REL). Rejected:
  bumping the version here, which would decide the first release's number
  inside the mechanism that only enables it.
- **D-4 (2026-11-01).** Dev-dependency edges between workspace crates
  (`aicortex-gate` on store and embed, `aicortex-store` on embed) become
  path-only with no version, so `cargo package` and `cargo publish` strip
  them; with a version, a crate could not publish before the crate it
  tests against, and the gate and store test cycle could never publish at
  all. `deny.toml` sets `allow-wildcard-paths = true`, which cargo-deny
  limits to dev-dependencies and unpublished crates, so a normal path
  dependency without a version is still refused. Rejected: dropping the
  cross-crate tests, which carry 013 AC-2 and 014's erasure proof.
- **D-5 (2026-11-01).** Each publishable crate carries its own
  `README.md` for FR-004's `readme`, so a crates.io page describes the
  crate rather than the whole service. Rejected: inheriting the
  repository README, which documents the app.
- **D-6 (2026-11-01, B-6's overlay).** The local registry overlay is a
  directory source that replaces crates.io for the consumer alone: the
  workspace's locked tree from `cargo vendor --locked`, with the five
  `.crate` files from `cargo package` unpacked into it as crates.io would
  serve them. The consumer names every dependency by version, resolves
  `--offline` against the overlay, and runs `cargo test --locked`. Cargo
  cannot overlay one registry on another for a user build, and an
  alternate registry would change the dependency source of the published
  crates' own edges. The consumer copies the fixture's `src` and `tests`
  and the one registry document its test reads by a workspace-relative
  path.
- **D-7 (2026-11-01).** The tag guard (`--guard-tag`) and the publish step
  (`--publish`) are modes of `scripts/release-check.sh`, so the territory
  stays the files B-1 to B-9 name and the self-test exercises the same
  guard the workflow runs. Publish skips a version already on crates.io,
  which makes B-4's recovery a rerun at the same tag.
- **D-8 (2026-11-01, B-8's commit).** The changelog section lands with the
  release pull request and the guard refuses a tag whose section is missing
  or lacks the I-3 sentence. A commit cannot name its own hash, so the
  tag's commit and the published versions are recorded after publication
  (AC-3), as `docs/release.md` step 5 says.
- **D-9 (2026-11-01, FR-001's fixture).** The store's migrations are Rust
  source (`src/migrations.rs`), not SQL files, so the self-test removes that
  file from a copy's store package (`exclude`) and expects packaging to
  refuse it. The static audit of `include_str!`, `include_bytes!`, and
  `#[path]` reads is exercised separately on a synthetic crate.

## Verification

```verify:cli
scripts/release-check.sh
scripts/release-check.sh --self-test
```
