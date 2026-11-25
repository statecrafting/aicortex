---
id: "063-statecraft-profile-28"
title: "Adopt Statecraft profile revision 28 with spec-spine staying at 0.32.0"
status: approved
kind: "tooling"
domain: "governance"
authors: ["Bartek Kus"]
risk: low
wave: 4
implementation: complete
created: "2026-11-08"
summary: >
  Authorizes the migration from Statecraft github-actions-rust revision 22 to
  revision 28 in one pull request. Revision 28's producer still links
  spec-spine-core 0.32.0, so the exact engine pin does not move and the
  policy-isolation guard sees no policy path beside the re-render. The
  revisions between add the reviewer contract, post-merge pushes under a
  required merge queue, an optional free-disk step, review severities and
  notes, and a policy guard that ignores a self-pin and the owning spec.
  Aicortex keeps its ordinary Rust code gate, corpus attestation and spec DAG
  jobs, and declares no merge queue or free-disk parameter.
amends:
  - "001-agentic-harness"
depends_on:
  - "001-agentic-harness"
  - "061-statecraft-profile-22"
  - "062-statecraft-derived-directory"
obligations:
  - id: "R-1"
    kind: requirement
    text: "Profile revision 28 replaces revision 22 while the exact spec-spine pin stays =0.32.0, the ordinary Rust code gate, corpus attestation and spec DAG remain required, and the derived directory stays at .statecraft/derived/."
    anchor: "3-migration-contract"
---

# 063: Statecraft profile revision 28

## 1. Purpose

The owner decided on 2026-11-08 to move aicortex from Statecraft
github-actions-rust revision 22 to revision 28 at once, rather than one
revision at a time. This spec records Aicortex's migration consent for that
move. It amends the revision 22 binding recorded by spec 061 and spec 001
without rewriting that approved history. No Aicortex product behavior or
acceptance authority is added.

## 2. Territory

This amendment owns no product code and grants no implementation claim. It
changes the adopted setup-profile revision and nothing else. The managed
files it regenerates stay in spec 001's authority, and existing owning specs,
workspace boundaries and specialized verification acceptance remain unchanged.

## 3. Migration contract

- The move is one pull request. Revision 28's producer links spec-spine-core
  0.32.0, the version `spec-spine.toml` already pins, so no engine pin moves
  and the pull request changes no policy path of the revision 22 guard
  (`spec-spine.toml`, any `Cargo.toml` or `Cargo.lock`, `deny.toml`,
  `docs/adoption/spec-spine.md`). The shards live under `.statecraft/derived/`
  (spec 062), which the guard ignores.
- Managed setup artifacts are regenerated with the revision 28 producer
  through `statecraft-cli init plan` and `init apply --plan <identity>`, never
  edited by hand. The setup parameters are the recorded ones, unchanged: the
  extra required jobs `corpus-attestation` and `spec-dag`, the authored
  content checker, enforced coverage, the per-commit gate, signed commits,
  `fail_on_unresolved: false` and `@bartekus` code ownership. Every parameter
  a later revision added keeps its default (D-1).
- The profile newly renders `.statecraft/REVIEW.md`, the reviewer's contract,
  inside the `.statecraft/` claim of spec 001.
- Every required job must still pass through the required `ci-gate`;
  cancellation, failure and an unexpected skip do not pass. The owner
  Environment approval path and the existing secret references are kept.

## 4. Acceptance criteria

- **AC-1.** The committed managed profile declares revision 28 and agrees
  with the recorded renderer identity, and a second `init plan` over the
  committed tree writes nothing.
- **AC-2.** The exact engine pin is unchanged at `=0.32.0` and every shard is
  fresh.
- **AC-3.** The ordinary Rust code job remains required together with the
  corpus attestation and spec DAG jobs and the profile's own required jobs.
- **AC-4.** `scripts/statecraft/verify-action-pins.sh` reports no finding
  over `.github/workflows/`.
- **AC-5.** Local governance, postcommit coupling and every required hosted
  check pass for the candidate before merge.
- **AC-6.** No product code, trust root, secret value, spec lifecycle other
  than this spec's own, or branch-protection rule is changed by the migration.

## 5. Out of scope

Any setup parameter change; declaring the merge queue (`remote.merge_queue`)
or `ci.code.free_disk`; any remote setting, which is the owner's to keep and is
reported, not applied; the comment in `spec-spine.toml` that names revision 22,
which is a policy path and moves with the next pin change.

## 6. Resolved decisions

- **D-1 (2026-11-08, implementation, no merge queue parameter).** Revision 24
  renders post-merge behavior, and requires every extra job's workflow to
  declare a `post_merge` input, only where the declared parameters say the
  default branch requires a merge queue. Aicortex's recorded parameters do not
  declare one, and declaring it is a remote-setting decision outside this
  migration, so the render is the revision 23 shape for pushes and the two
  extra workflows are unchanged. Declaring the queue later needs both
  workflows to take a boolean `post_merge` input first.

## 7. Approval basis

Owner consent is the explicit owner decision of 2026-11-08 to update to
revision 28 at once. This spec grants only that scoped migration. It lands
together with the migration, and the candidate must pass its declared local
and hosted acceptance before merge.

## 8. Implementation record

The renderer is statecraft-cli built from its `main` at
`f133913a98ceaa58ff47de1e628989022aff28ab`. The renderer binary's SHA-256 is
`35bc660fc03f3a8122eaed012344da9ac0c7007af38bea69abb30c127d074da1`. Its
producer is spec-spine-core 0.32.0, checksum
`973e77cfb443aa4de15fd98149a34ebd10756b8818f8aef0490e5eeb03f257e0`.
The rendered profile is revision 28 with identity
`ddb55e6ee7c1478b1c88463a3f2fa7a53b3f20aad492a7dfcae29cbbd4d21178`.
The reviewed setup plan identity is
`85f12f45451d2244e150a05dd4c521217edd585d51800faba615cd7f3a13e556`. It
replaces the managed workflows, scripts, policy, remote desired-state
document and CODEOWNERS, writes `.statecraft/REVIEW.md`, and withholds the
adopted `spec-spine.toml` and standards files.

## Verification

```verify:cli
make gate
scripts/spec-dag.sh
sh scripts/statecraft/verify-action-pins.sh
```
