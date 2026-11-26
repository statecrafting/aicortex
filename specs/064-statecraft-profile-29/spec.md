---
id: "064-statecraft-profile-29"
title: "Adopt Statecraft profile revision 29 with spec-spine staying at 0.32.0"
status: approved
kind: "tooling"
domain: "governance"
authors: ["Bartek Kus"]
risk: low
wave: 4
implementation: complete
created: "2026-11-26"
summary: >
  Authorizes the migration from Statecraft github-actions-rust revision 28 to
  revision 29 in one pull request. Revision 29's policy-isolation guard
  ignores, as new-crate, the manifest and lockfile blocks of a new workspace
  crate that adds no registry or git package, so a new crate lands in one
  pull request. The producer still links spec-spine-core 0.32.0, so the
  exact engine pin does not move. Aicortex keeps its ordinary Rust code gate,
  corpus attestation and spec DAG jobs, and every recorded setup parameter.
amends:
  - "001-agentic-harness"
depends_on:
  - "001-agentic-harness"
  - "063-statecraft-profile-28"
obligations:
  - id: "R-1"
    kind: requirement
    text: "Profile revision 29 replaces revision 28 while the exact spec-spine pin stays =0.32.0, the ordinary Rust code gate, corpus attestation and spec DAG remain required, and the derived directory stays at .statecraft/derived/."
    anchor: "3-migration-contract"
---

# 064: Statecraft profile revision 29

## 1. Purpose

The owner decided on 2026-11-26 to adopt Statecraft github-actions-rust
revision 29 (statecraft-cli spec 073, ratified the same day). Under revision
28 a new workspace crate could not land at all: the workspace members glob
makes a manifest alone break the build, and the policy-isolation guard
refused the manifest beside its sources, so #131 and #133 needed the owner
to merge them by hand. This spec records Aicortex's migration consent. It
amends the revision 28 binding recorded by spec 063 and spec 001 without
rewriting that approved history. No Aicortex product behavior or
acceptance authority is added.

## 2. Territory

This amendment owns no product code and grants no implementation claim. It
changes the adopted setup-profile revision and nothing else. The managed
files it regenerates stay in spec 001's authority, and existing owning
specs, workspace boundaries and specialized verification acceptance remain
unchanged.

## 3. Migration contract

- The move is one pull request. Revision 29's producer links
  spec-spine-core 0.32.0, the version `spec-spine.toml` already pins, so no
  engine pin moves and the pull request changes no policy path.
- Managed setup artifacts are regenerated with the revision 29 producer
  through `statecraft-cli init plan` and `init apply --plan <identity>`,
  never edited by hand, with one exception (D-1). The setup parameters are
  the recorded ones, unchanged, and every parameter a later revision added
  keeps its default.
- Every required job must still pass through the required `ci-gate`;
  cancellation, failure and an unexpected skip do not pass. The owner
  Environment approval path and the existing secret references are kept.

## 4. Acceptance criteria

- **AC-1.** The committed managed profile declares revision 29 and agrees
  with the recorded renderer identity.
- **AC-2.** The exact engine pin is unchanged at `=0.32.0` and every shard is
  fresh.
- **AC-3.** The ordinary Rust code job remains required together with the
  corpus attestation and spec DAG jobs and the profile's own required jobs.
- **AC-4.** `scripts/statecraft/verify-action-pins.sh` reports no finding
  over `.github/workflows/`.
- **AC-5.** No product code, trust root, secret value, spec lifecycle other
  than this spec's own, or branch-protection rule is changed.

## 5. Out of scope

Any setup parameter change; declaring the merge queue (`remote.merge_queue`)
or `ci.code.free_disk` (063 D-1 still holds); any remote setting.

## 6. Resolved decisions

- **D-1 (2026-11-26, implementation, newer action pins kept).** The plan
  named `statecraft-ci.yml` and `statecraft-ai-review.yml` as customized:
  after revision 28, Dependabot moved `actions/checkout` to v7.0.1 and
  `actions/upload-artifact` to v7.0.2 (#114). The rendered files are taken
  whole from the revision 29 intended bytes with those two newer digests
  kept, rather than moving the actions back to the profile's v7.0.0 and
  v7.0.1. The rest of each file is the render byte for byte, and every
  action stays pinned by digest (AC-4).

## 7. Approval basis

Owner consent is the explicit owner decision of 2026-11-26 to adopt
revision 29. This spec grants only that scoped migration. It lands together
with the migration, and the candidate must pass its declared local and
hosted acceptance before merge.

## 8. Implementation record

The renderer is statecraft-cli built from its `main` at
`68832a2a97b9b53151a1f2e365a13ef65a5a6d39` (#443, revision 29). The renderer binary's SHA-256 is
`6aa44ebcf55eb347f508fa4632ff964c5354d7098432bdf83ad71939210951cd`. Its
producer is spec-spine-core 0.32.0. The rendered profile is revision 29
with identity
`9d5bb0e25975598a07bcb7ed7f0466a25dd725508cd050dbdbbec6f4950dcebe`. The
reviewed setup plan identity is
`1e105c67d28e96e132b2e4628574dd4006416a84a4fe7417a3751cbe82a27487`. It
replaces the managed scripts, policy, remote desired-state document,
CODEOWNERS and `.statecraft/REVIEW.md`, renders the two workflows under
D-1, and withholds the adopted `spec-spine.toml` and standards files.

## Verification

```verify:cli
make gate
scripts/spec-dag.sh
sh scripts/statecraft/verify-action-pins.sh
```
