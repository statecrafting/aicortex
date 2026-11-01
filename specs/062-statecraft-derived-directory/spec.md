---
id: "062-statecraft-derived-directory"
title: "Move the committed derived directory to .statecraft/derived/"
status: approved
kind: "tooling"
domain: "governance"
authors: ["Bartek Kus"]
risk: low
wave: 4
implementation: in-progress
created: "2026-10-31"
summary: >
  Authorizes moving spec-spine's committed derived directory from .derived/
  to .statecraft/derived/, the path Statecraft profile revision 22 treats as
  generated. Revision 22's policy guard ignores .statecraft/derived/ but not
  .derived/, so while the shards live at .derived/ a spec-spine pin move
  cannot regenerate them in its own pull request (spec 061 D-4). The move
  lands as three pull requests because the guard keeps spec-spine.toml apart
  from every other non-generated path.
amends:
  - "001-agentic-harness"
depends_on:
  - "001-agentic-harness"
  - "061-statecraft-profile-22"
obligations:
  - id: "R-1"
    kind: requirement
    text: "spec-spine.toml [layout] derived_dir is .statecraft/derived, every committed registry and codebase-index shard lives under .statecraft/derived/ and is fresh, no file remains under .derived/, and every tool, hook and document that names the derived directory names .statecraft/derived/."
    anchor: "3-migration-contract"
---

# 062: The derived directory moves to .statecraft/derived/

## 1. Purpose

The owner decided on 2026-10-31 to move the committed derived directory to
`.statecraft/derived/`, the follow-up spec 061 D-4 left for its own decision.
Statecraft profile revision 22 already treats `.statecraft/derived/` as
generated: its policy guard ignores it, its AI review excludes it, and its
rendered `.gitignore` block and `.statecraft/AGENTS.md` describe it. With the
shards there, a future spec-spine pin move regenerates them in the same pull
request as the pin. No Aicortex product behavior or acceptance authority is
added.

## 2. Territory

This amendment owns no product code and grants no implementation claim. It
changes one layout key and one resolver exclusion in `spec-spine.toml`, the
location of the committed shards, and the tooling and prose in spec 001's
authority that name the derived directory. The managed Statecraft files are
not re-rendered.

## 3. Migration contract

Revision 22's `guard-policy-files.sh` refuses a pull request that changes
`spec-spine.toml` beside any path other than `.statecraft/derived/` and
`.statecraft/environment.json`. Deleting a shard under `.derived/` is such a
path, and so is any spec edit. The move therefore lands in three pull
requests, each green on its own:

- **Preparation.** This spec, the spec 001 decision D-20, and tooling that
  reads either location: the merge-driver attributes cover both trees, `make
  attest` writes under the configured derived directory, and the pull request
  gate hook checks both trees for uncommitted shards.
- **Configuration.** `spec-spine.toml [layout] derived_dir` becomes
  `.statecraft/derived` and its `resolver_exclusions` entry `.derived`
  becomes `.statecraft/derived`; every shard is regenerated under
  `.statecraft/derived/`. Nothing else changes, so the guard passes. The
  coupling gate asks for an owning spec edit beside `spec-spine.toml`, which
  the guard forbids; the owner approved a `Spec-Drift-Waiver` for this pull
  request on 2026-10-31, citing this spec (D-1).
- **Cleanup.** The stale `.derived/` tree is deleted, the tooling drops the
  old location, every document that names the derived directory moves to
  `.statecraft/derived/`, spec 001 records the implementation, and this spec
  is marked complete.

## 4. Acceptance criteria

- **AC-1.** `spec-spine.toml` declares `derived_dir = ".statecraft/derived"`,
  and `spec-spine check` reports both shard trees fresh.
- **AC-2.** No tracked file remains under `.derived/`, and no tool, hook or
  document outside the dated history of specs 000, 001, 060 and 061 names
  `.derived/` as the live derived directory.
- **AC-3.** A pull request that changes only `spec-spine.toml` and the
  shards passes `scripts/statecraft/guard-policy-files.sh`.
- **AC-4.** Local governance, postcommit coupling and every required hosted
  check pass for each pull request before merge.

## 5. Out of scope

Any re-render of the Statecraft profile, any setup parameter, the spec-spine
pin, and any remote setting.

## 6. Resolved decisions

- **D-1 (2026-10-31, owner decision, the coupling waiver).** The coupling
  gate refuses a change to `spec-spine.toml` without an authoring edit to an
  owning spec (C-001: specs 001 and 010), and the policy guard refuses that
  edit in the same pull request. A local trial of the configuration pull
  request passed the guard (1 policy path, 108 generated, 0 other), `check`
  and `lint --fail-on-warn`, and failed coupling only. The owner approved a
  `Spec-Drift-Waiver` for the configuration pull request alone; this spec and
  spec 001 D-20, landed before it, are the spec edits the waiver points to.
- **D-2 (2026-10-31, implementation, the secret scan allowlist).** The first
  configuration candidate failed revision 22's `secret-scan` job: gitleaks'
  `generic-api-key` rule matched three registry shards
  (`020-http-api-and-scopes`, `025-evidence-delivery-and-token-budget`,
  `045-durable-coordination-protocol`), where a section anchor maps to the
  SHA-256 digest of the section. The same lines had passed at `.derived/`
  only because the job scans the pull request's own commits. The profile
  leaves the allowlist to the project, so a committed `.gitleaks.toml`, owned
  by spec 001 and landed ahead of the configuration pull request, keeps the
  default rules and allows only a value of exactly 64 lowercase hex digits in
  a path under `.statecraft/derived/`.

## 7. Approval basis

Owner consent is the explicit owner decision of 2026-10-31 to move the
derived directory to `.statecraft/derived/` and to approve the coupling
waiver for the configuration pull request. This spec grants only that scoped
move.

## Verification

```verify:cli
make gate
scripts/spec-dag.sh
test ! -e .derived
```
