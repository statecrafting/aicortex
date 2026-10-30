---
id: "061-statecraft-profile-22"
title: "Adopt Statecraft profile revision 22 with spec-spine pinned at 0.32.0"
status: approved
kind: "tooling"
domain: "governance"
authors: ["Bartek Kus"]
risk: low
wave: 4
implementation: complete
created: "2026-10-30"
summary: >
  Authorizes the migration from Statecraft github-actions-rust revision 21 to
  revision 22 together with the exact spec-spine pin move from 0.31.0 to
  0.32.0. Revision 22's producer links spec-spine-core 0.32.0 and refuses any
  other pin, so the two moves land together. Revision 22 adds four required
  jobs (supply chain, workflow lint, ast-grep scan, secret scan); its supply
  chain job replaces the project's extra job of the same name. Aicortex keeps
  its ordinary Rust code gate and the corpus attestation and spec DAG jobs.
amends:
  - "001-agentic-harness"
depends_on:
  - "001-agentic-harness"
  - "060-statecraft-profile-21"
obligations:
  - id: "R-1"
    kind: requirement
    text: "Profile revision 22 replaces revision 21 while the exact spec-spine pin is =0.32.0, the ordinary Rust code gate, corpus attestation and spec DAG remain required, the supply chain check is carried by the profile's required supply-chain job, and the derived directory stays at .derived/."
    anchor: "3-migration-contract"
---

# 061: Statecraft profile revision 22

## 1. Purpose

The owner decided on 2026-10-30 to move aicortex to spec-spine 0.32.0, which
needs Statecraft github-actions-rust revision 22: statecraft-cli links
spec-spine-core 0.32.0 from its pull request #358 and registers revision 22
from its pull request #359. This spec records Aicortex's migration consent for
that move. It amends the revision 21 binding recorded by spec 060 and spec 001
without rewriting that approved history. No Aicortex product behavior or
acceptance authority is added.

## 2. Territory

This amendment owns no product code and grants no implementation claim. It
changes the adopted setup-profile revision, one setup parameter, the exact
engine pin, and the action references of the project's own workflows. The
managed files it regenerates stay in spec 001's authority, and existing owning
specs, workspace boundaries and specialized verification acceptance remain
unchanged.

## 3. Migration contract

The engine pin and the profile revision move in two pull requests, pin
first. Revision 22's producer links spec-spine-core 0.32.0 and withholds the
profile while `spec-spine.toml` pins any other version, so the pin must be at
0.32.0 before the profile moves. Revision 22's `guard-policy-files.sh` refuses
a pull request that changes `spec-spine.toml` beside any other path apart
from `.statecraft/derived/` and `.statecraft/environment.json`, and on a base
that carries no guard it runs the candidate's copy, so the profile move cannot
carry the pin (D-4). The pin move runs under the revision 21 workflows, which
have no guard.

- **The pin pull request** moves `spec-spine.toml`, regenerates every shard,
  moves the sites that state the pin, and adds spec 001 D-19 and this spec.
- **The profile pull request** re-renders the managed files at revision 22
  over a tree already pinned at 0.32.0, brings the project's workflows to the
  new lint, records the implementation in spec 001, and marks this spec
  complete.

- `spec-spine.toml [meta] required_version` becomes `=0.32.0`, and every site
  that states the pin moves with it (spec 001 D-19).
- Every registry and codebase-index shard is regenerated with 0.32.0.
- Managed setup artifacts are regenerated with the revision 22 producer
  through `statecraft-cli init plan` and `init apply --plan <identity>`, never
  edited by hand. The setup parameters are supplied as a reviewed setup input
  that keeps every recorded value except one: the extra required job
  `supply-chain` (`.github/workflows/cargo-deny.yml`) is removed, because
  revision 22 renders a required job of that name and refuses a project job
  that collides with it (D-1). The extra required jobs `corpus-attestation`
  and `spec-dag`, the authored content checker over files and pull request
  text, enforced coverage, the per-commit gate, signed commits, and
  `@bartekus` code ownership are kept.
- Revision 22's `workflow-lint` job holds every workflow under
  `.github/workflows/`, rendered or not, to an action reference by 40-hex
  commit digest with a `# <ref>` comment and to a clean offline zizmor run.
  The project's own workflows are brought to that standard without changing
  what they run (D-2).
- Aicortex keeps the ordinary Rust code gate. Every required job must still
  pass through the required `ci-gate`; cancellation, failure and an
  unexpected skip do not pass. The owner Environment approval path and the
  existing secret references are kept.
- The derived directory stays at `.derived/`. The move to
  `.statecraft/derived/` remains separate work needing its own owner
  decision.

## 4. Acceptance criteria

- **AC-1.** The committed managed profile declares revision 22 and agrees
  with the recorded renderer identity, and a second `init plan` over the
  committed tree writes nothing.
- **AC-2.** The exact engine pin is `=0.32.0` in `spec-spine.toml` and every
  site that states it, and every shard is fresh under 0.32.0.
- **AC-3.** The ordinary Rust code job remains required together with the
  corpus attestation and spec DAG jobs, and the supply chain check is the
  profile's required `supply-chain` job.
- **AC-4.** `scripts/statecraft/verify-action-pins.sh` and zizmor 1.31.1 in
  offline mode report no finding over `.github/workflows/`.
- **AC-5.** Local governance, postcommit coupling and every required hosted
  check pass for the integration candidate before merge.
- **AC-6.** No product code, trust root, secret value, spec lifecycle other
  than this spec's own, or branch-protection rule is changed by the migration.

## 5. Out of scope

The `.derived/` to `.statecraft/derived/` move; any setup parameter beyond
the retired `supply-chain` extra job; any remote setting. The remote
obligations revision 22 lists are the owner's to keep and are reported, not
applied, by this change. Adding `.cargo/audit.toml`, `supply-chain/config.toml`
or `sgconfig.yml`, which would switch on the conditional halves of the new
jobs, is separate work.

## 6. Resolved decisions

- **D-1 (2026-10-30, implementation, the supply-chain name).** The first
  revision 22 plan refused: `ci.extra_required_jobs: supply-chain is a job the
  profile renders`. The project's job ran `cargo-deny-action` against the
  committed `deny.toml`; the profile's job runs `cargo deny --locked check
  all` against the same file, plus `cargo audit` and `cargo vet` when their
  configuration exists, with digest-verified tools. The profile's job is a
  superset, so the project's extra job is retired through the setup input
  rather than renamed. `cargo deny --locked check all` passes locally on this
  tree. `.github/workflows/cargo-deny.yml` stays claimed by spec 001 and is no
  longer called by `ci-gate`.
- **D-2 (2026-10-30, implementation, the project's workflows).** The action
  pin check found twelve tag references in `cargo-deny.yml`,
  `corpus-attestation.yml`, `govern.yml` and `spec-dag.yml`. Each is replaced
  by the commit its tag resolved to on 2026-10-30 (checkout v7.0.1,
  upload-artifact v7.0.2, cargo-deny-action v2.1.1, rust-cache v2.9.3), so
  the bytes each step runs are unchanged. zizmor then reported seven
  `artipacked` findings, one per checkout; none of those jobs uses git
  credentials after checkout, so each sets `persist-credentials: false`.
- **D-3 (2026-10-30, implementation, engine 0.32).** Regenerating under 0.32.0
  adds `declaresAcceptance` to each registry shard and moves the registry
  schema from 1.10.0 to 1.11.0; `check` and `lint` stay clean at
  `--fail-on-warn` with no corpus edit.
- **D-4 (2026-10-30, implementation, two pull requests).** The first
  candidate carried the pin and the profile together. Its governance job ran
  the candidate's `guard-policy-files.sh`, because the base carries none, and
  refused: `spec-spine.toml` beside 72 other paths. The change is split, pin
  first. In this repository the regenerated shards live under `.derived/`,
  which the guard does not treat as generated, so a later pin move under
  revision 22 cannot regenerate them in the same pull request; moving the
  derived directory to `.statecraft/derived/` (out of scope here) is what
  makes a lone pin pull request possible again.

## 7. Approval basis

Owner consent is the explicit owner decision of 2026-10-30 to move to
spec-spine 0.32.0 through profile revision 22. This spec grants only that
scoped migration. It lands together with the migration, and the candidate
must pass its declared local and hosted acceptance before merge.

## 8. Implementation record

The renderer is statecraft-cli built from its `main` at
`18fb8ecc9478367428f79e8dca7856006829370f`. The renderer binary's SHA-256 is
`51a456bbe7585f04c9fa5a3fc7eee2aad54d17a535de959d560b30a322ae6f86`. Its
producer is spec-spine-core 0.32.0, checksum
`973e77cfb443aa4de15fd98149a34ebd10756b8818f8aef0490e5eeb03f257e0`.
The rendered profile is revision 22 with identity
`6da3ede78368133fe8f463ac341a8ea0ce8e959c0c059066f50c8a89253a4c88`.

The reviewed setup plan identity, over the tree the pin pull request left
at `=0.32.0`, is
`8e3850e6d28de4a50b8df8b4c78c1ff0e2a3c837c2bd0e58b1aee97a2b8fa9a8`. It
renders the same managed bytes as the first, combined candidate (plan
`spine-init:8a29ae5f4c565299e15403e93cf072941fcec2aeaa2776ede8fe7252d85f70ca`),
and it replaces the managed workflows, scripts, policy, remote desired-state
document and CODEOWNERS, adds `scripts/statecraft/guard-policy-files.sh` and
`scripts/statecraft/verify-action-pins.sh`, and withholds the adopted
`spec-spine.toml` and standards files. Local
verification and the hosted checks are reported in the pull request.

## Verification

```verify:cli
make gate
scripts/spec-dag.sh
sh scripts/statecraft/verify-action-pins.sh
```
