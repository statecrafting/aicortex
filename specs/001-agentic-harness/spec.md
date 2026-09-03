---
id: "001-agentic-harness"
title: "Agentic engineering harness: session protocol, skills, agents, hooks, and the gate"
status: approved
kind: "governance"
domain: "governance"
created: "2026-09-03"
authors: ["Bartek Kus"]
implementation: complete
risk: high
wave: 1
depends_on:
  - "000-aicortex-bootstrap"
establishes:
  - "AGENTS.md"
  - "CLAUDE.md"
  - "Makefile"
  - "spec-spine.toml"
  - ".mcp.json"
  - "standards/spec/contract.md"
  - "standards/spec/templates/"
  - ".claude/settings.json"
  - ".claude/agents/"
  - ".claude/rules/"
  - ".claude/skills/"
  - ".github/workflows/govern.yml"
  - ".github/dependabot.yml"
  - "scripts/verify-spec.sh"
  - "scripts/spec-dag.sh"
references:
  - { unit: { kind: file, path: "docs/design/00-lineage.md" }, role: context }
summary: >
  The machinery that turns the corpus into work: AGENTS.md as the
  cross-agent session protocol and backlog discipline, CLAUDE.md as the
  Claude Code overlay, five behavioral rules of which two are path-scoped,
  four subagents, fifteen skills, settings hooks that recompile and refuse,
  one Makefile that is the single definition of the gate, and a CI workflow
  that runs the same targets. This spec is complete on arrival: the harness
  exists before the first line of product code.
---

# 001: Agentic engineering harness

## 1. Purpose

The corpus is only a design until something drives it. This spec owns the
surfaces that make a driven session behave the same way every time: where
it starts, how it picks work, what it must not do, and what it must prove
before it stops. It is carried from the harness generation used by `hqgit`,
with the crate map, the invariants, and the taxonomies replaced by this
product's.

## 2. Territory

Every governance surface at the repository root plus `.claude/`,
`.github/`, `scripts/`, and `standards/spec/{contract,templates}`. The
constitution is 000's; this spec owns the operational summary of it.

## 3. Behavior

- **B-1 (protocol location).** `AGENTS.md` is the single source for the
  session-init protocol and the backlog discipline, read by every agent
  runtime through the AGENTS.md standard. The `/init` skill dispatches to
  it and never carries a second copy of the protocol.
- **B-2 (the gate is the Makefile).** `make ci` is the definition of what
  CI validates. Every target is guarded so the composite is green on a
  specify-only tree, because before spec 010 lands there is no
  `Cargo.toml`. CI runs the same targets with `compile --check`.
- **B-3 (rules).** Three standing rules load at init (orchestrator,
  governed artifact reads, the coherence guard) and two path-scoped rules
  load on touch (`memory-invariants` for the admission, understanding,
  recall, and MCP crates; `build-commands` for anything under `crates/`,
  `apps/`, `eval/`, `docker/`, or `deploy/`).
- **B-4 (agents).** `architect`, `explorer`, `implementer`, and `reviewer`,
  all self-contained, with the read-only ones actually read-only.
- **B-5 (hooks).** `.claude/settings.json` recompiles the registry after a
  spec edit, checks index staleness after a hashed-input edit, blocks `gh
  pr create` when the coupling gate is red, and blocks `git push` to
  `main`.
- **B-6 (one spec per session).** A session implements one spec start to
  finish and stops. Territory that cannot fit one session is a signal to
  split the spec, not to run longer.
- **B-7 (decisions).** A choice the spec is silent on is recorded as a
  dated `D-n` entry in that spec's `## 7. Resolved decisions`, with a copy
  in `data/orchestrator/decision-dropbox/` when the session is driven.
- **B-8 (no npm).** There is no `package.json` or `tsconfig.json` at the
  repository root. This is deliberate and is the reason for D-1.

## 4. Functional requirements

- **FR-001.** `make spine` exits 0 on the specify-only tree.
- **FR-002.** `make ci` exits 0 on the specify-only tree and reports each
  cargo target as skipped rather than failing.
- **FR-003.** `scripts/spec-dag.sh` exits non-zero on a cycle and on a
  dependency that names a higher-numbered spec.
- **FR-004.** `scripts/verify-spec.sh <id>` runs every `verify:cli` line of
  that spec and propagates the first non-zero exit.
- **FR-005.** The em-dash hook refuses a write that introduces U+2014.

## 5. Acceptance criteria

- **AC-1.** `make ci` exits 0 on a clean checkout with `spec-spine` 0.11.0
  on `PATH`.
- **AC-2.** `scripts/spec-dag.sh` reports the corpus acyclic with every
  dependency lower-numbered.

## 6. Out of scope

The constitution and the corpus contract's normative content (000). The
orchestrator that drives sessions, which lives in `claude-observatory` and
consumes this repository as a registered target.

## 7. Resolved decisions

- **D-1 (2026-09-03, bootstrap).** No `package.json` or `tsconfig.json` at
  the repository root, even though the orchestrator's build stage probes
  for one to decide its post-session gate. A stub manifest would make a
  Rust repository lie about being a Bun project to satisfy a tool. The
  correct fix is the orchestrator's project gate contract
  (`claude-observatory` spec 041), which reads `make ci` from the registry;
  until it ships, cargo correctness reaches the pipeline through `make ci`
  inside the session and through CI in the shepherd stage.
- **D-2 (2026-09-03, bootstrap).** The harness is copied from `hqgit`'s
  generation rather than written fresh, so that a contributor moving
  between the repositories in this family finds the same protocol, the same
  skill names, and the same failure messages.

## Verification

```verify:cli
make spine
scripts/spec-dag.sh
```
