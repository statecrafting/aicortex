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

- **AC-1.** `make ci` exits 0 on a clean checkout with `spec-spine` 0.14.0
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
- **D-3 (2026-09-05, first CI runs).** The CI mirror of B-2 in
  `govern.yml` guards its cargo jobs with an output of the `spine` job, not
  `hashFiles` in a job-level `if`. GitHub allows `hashFiles` only inside a
  step (a job-level `if` is evaluated before any checkout), and the
  workflow fails at startup with `calling function "hashFiles" is not
  allowed here`, which reports as a run with no checks rather than as a
  failed gate; every govern run on this repository failed that way until
  this fix. The `spine` job probes for `Cargo.toml` and `deny.toml` after
  its checkout and publishes `has_cargo` and `has_deny`; the `cargo` and
  `deny` jobs gate on those. The guard's meaning is unchanged. The same
  review dropped the dependabot `npm` entry for `/web`, which D-1 already
  rules out and which failed on every scheduled run, and replaced the
  hqgit vocabulary the D-2 copy carried into the skills and agents
  (golden vectors, `ledger-guardian`, `trust-reviewer`, L0/L1, `web/`)
  with this product's memory invariants and crates. Learned from rahi's
  001 D-4 and hqgit's 001 D-3, where the identical workflow first failed.

- **D-4 (2026-09-06, pin bump).** The `spec-spine` pin moves from 0.11.0
  to 0.14.0 in every site that states it (`govern.yml`, `AGENTS.md`,
  `README.md`, `/setup`, the architect agent, AC-1). The corpus was
  verified byte-compatible first: 0.14.0's `compile --check` and `index
  check` both report fresh against shards written by 0.11.0. What the bump
  buys: `registry plan` (spec-spine 038, which `/next` reimplemented in
  Python), `--json` verdicts on the gate verbs (037), `layout.state_dir`
  (039), the `depends_on` cycle refusal (033), and the lifecycle fixes
  (041, 044, 045) this specify-first corpus lives inside. `spec-spine
  index` now prints the `W-001` warnings it always recorded; with 26 specs
  pending that is one line per not-yet-written unit (248 today) and is the
  expected state, not a defect. Follow-ons (`registry plan` in the init
  reads, `state_dir`, retiring the Python in `/next`) are their own change.

- **D-5 (2026-09-06, kit adoption).** The fifteen skills under
  `.claude/skills/` are the spec-spine kit's own (spec-spine spec 048),
  taken byte for byte, and the three standing rules are the kit's spec 047
  text. The kit moved every project fact out of the skills into
  `AGENTS.md` and the path-scoped rules, which this repository already
  held (`make spine`, `make ci`, the 0.14.0 pin, `memory-invariants`), so
  nothing was lost in the swap and a future kit update is a copy. This
  also retires the copy residue the audit found: the `build` skill's
  dangling `ledger-invariants` and `trust-invariants` references, the
  `spec` skill's domain enum copied from another corpus (the skill now
  reads `[domains] allowed` from `spec-spine.toml`), and the
  `code-review` heading mismatch. In substance: `/next` wraps
  `spec-spine registry plan` and drops the Python readiness script (the
  D-4 follow-on), `/code-review` uses `compile --check` so a review never
  writes, `/commit` carries the session-link and em-dash bans, and
  `scripts/verify-spec.sh` is the kit's copy. B-5's hooks are unchanged:
  the kit's hooks now read and never write (spec-spine spec 046), and
  porting them changes what B-5 requires of the `PreToolUse` and `Stop`
  hooks, which is an amendment for a human to file, not a mid-build edit.

- **D-6 (2026-09-09, hashed inputs).** Nine of the thirteen `[index]
  extra_hashed_inputs` patterns ended in `**`, which enumerates
  directories and contributes no bytes to any content hash. Six of the
  nine cover directories that exist and hold thirty files between them
  (`standards/`, `.github/workflows/`, `.claude/agents/`,
  `.claude/rules/`, `.claude/skills/`, `docs/design/`), so those files
  were folded into the global scalar by nothing at all: an edit to
  `standards/spec/contract.md`, or to a standing rule, staled no shard
  and passed `index check` clean. The other three (`docker/`, `deploy/`,
  `eval/`) name directories this repository has not created yet, so they
  cost nothing today and would have stayed silently dead on the day they
  arrive, which is the worse failure of the two. The bare filenames in
  the same list (`AGENTS.md`, `CLAUDE.md`, `Makefile`,
  `.claude/settings.json`) were never affected and are unchanged. Every
  glob is rewritten as `**/*`, and `scripts/**/*`, `.mcp.json` and
  `.github/dependabot.yml` are added for the claimed paths that no
  pattern covered at all. Measured with the pinned 0.14.0 binary, which is
  what CI recomputes against: before the change, appending a line to
  `standards/spec/contract.md` left `index check` at exit 0; after it, the
  same edit exits 2. Regenerating rewrites all 30 index shards, which is
  the one-time cost of the patterns finally covering bytes and the
  evidence the hole was real rather than cosmetic. The `spec-spine` pin is
  untouched here, and B-2 is unaffected: `make ci` and CI still run the
  same targets. No spec text names the hashed-input patterns, so this is a
  choice the corpus was silent on and B-7's dated decision entry is the
  right instrument.

## Verification

```verify:cli
make spine
scripts/spec-dag.sh
```
