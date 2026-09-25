---
id: "048-one-spec-per-pull-request"
title: "One spec per pull request; a session may land several specs in build order"
status: approved
kind: "governance"
domain: "governance"
created: "2026-09-25"
authors: ["Bartek Kus"]
implementation: complete
risk: medium
wave: 4
depends_on:
  - "001-agentic-harness"
extends:
  - { spec: "001-agentic-harness", unit: "AGENTS.md", nature: additive }
  - { spec: "001-agentic-harness", unit: "CLAUDE.md", nature: additive }
  - { spec: "001-agentic-harness", unit: "README.md", nature: additive }
  - { spec: "001-agentic-harness", unit: ".claude/rules/", nature: additive }
  - { spec: "001-agentic-harness", unit: ".claude/agents/", nature: additive }
amends:
  - "001-agentic-harness"
amends_sections:
  - "3-behavior"
references:
  - { unit: { kind: file, path: "standards/spec/constitution.md" }, role: context }
summary: >
  The owner's decision of 2026-09-25 replaces the repository rule that code
  lands one spec per session with one spec per pull request: a session may
  land several specs, each on its own branch and in its own pull request,
  in build order. This spec amends spec 001 B-6 and carries the new sentence
  into the harness text that repeated the old one (AGENTS.md, CLAUDE.md,
  README.md, the orchestrator rule, the architect agent, and constitution
  principle III).
---

# 048: One spec per pull request

## 1. Purpose

Spec 001 B-6 says a session implements one spec start to finish and stops.
The rule served two ends: a reviewable unit of change, and a territory small
enough to finish. The pull request already carries the first end, and a
spec's territory carries the second. Tying the unit to a session instead
made a session stop after one merge even when the next spec in build order
was ready and the session had room for it. The owner decided on 2026-09-25
that the unit is the pull request.

## 2. Territory

No code. This spec amends 001 section 3 (B-6) through its `amends` edge and
extends the harness files 001 establishes that restate B-6: `AGENTS.md`,
`CLAUDE.md`, `README.md`, `.claude/rules/orchestrator-rules.md` and
`.claude/agents/architect.md`. It also edits constitution principle III,
which is not frozen. The ten skills under `.claude/skills/` are the
spec-spine kit's, byte for byte (001, spec-spine spec 081), and are not
edited here.

## 3. Behavior

- **B-1 (one spec per pull request).** Each spec is implemented on its own
  branch, cut from the latest default branch, and lands through its own
  pull request. A pull request carries exactly one spec's implementation.
  Pull requests are not stacked.
- **B-2 (several per session, in build order).** A session may land more
  than one spec. It takes them in build order: each next spec is picked by
  the rule in `AGENTS.md` "Working the backlog" step 1 against the default
  branch after the previous one merged, never against an unmerged branch.
- **B-3 (the split signal survives).** Territory that cannot fit one pull
  request is a signal to split the spec, not to grow the pull request.
- **B-4 (amends 001 B-6).** Where 001 B-6 says "one spec per session", this
  spec's B-1 to B-3 govern.

## 4. Functional requirements

- **FR-001.** No harness file this spec extends, and neither
  `standards/spec/constitution.md` nor `standards/spec/contract.md`, still
  states the one-spec-per-session rule.

## 5. Acceptance criteria

- **AC-1.** `make gate` exits 0.
- **AC-2.** `git grep -n -i "per session"` over `AGENTS.md`, `CLAUDE.md`,
  `README.md`, `standards/`, `.claude/rules/` and `.claude/agents/` finds
  nothing.

## 6. Out of scope

The kit skills (`/build`, `/next`) still describe one spec per session in
their own words; they change only when the spec-spine kit changes, and a
reader follows `AGENTS.md`, which the skills defer to for project facts.

## 7. Resolved decisions

- **D-1 (2026-09-25, owner).** "One spec per pull request, several per
  session." The repository rule "code arrives / lands one spec per session"
  becomes "one spec per pull request; a session may land several specs in
  build order". The owner's approval of this decision is the approval of
  this spec.

## Verification

```verify:cli
make gate
! git grep -n -i "per session" -- AGENTS.md CLAUDE.md README.md standards .claude/rules .claude/agents
```
