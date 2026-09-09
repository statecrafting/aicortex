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
  - ".githooks/"
  - ".gitattributes"
  - "scripts/spec-dag.sh"
references:
  - { unit: { kind: file, path: "docs/design/00-lineage.md" }, role: context }
summary: >
  The machinery that turns the corpus into work: AGENTS.md as the
  cross-agent session protocol and backlog discipline, CLAUDE.md as the
  Claude Code overlay, six behavioral rules of which three are path-scoped,
  four subagents, ten skills, settings hooks that report and refuse, one
  Makefile that is the single definition of the gate and splits its reading
  half from its writing half, and a CI workflow that runs the same targets.
  This spec is complete on arrival: the harness exists before the first line
  of product code.
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
  runtime through the AGENTS.md standard. The `/prime` skill dispatches to
  it and never carries a second copy of the protocol.
- **B-2 (the gate is the Makefile).** `make ci` is the definition of what
  CI validates. Every target is guarded so the composite is green on a
  specify-only tree, because before spec 010 lands there is no
  `Cargo.toml`. The chain is split: `make gate` reads and never writes,
  `make refresh` is the writing half, and `make spine` is the two in order
  for a session that can commit the regenerated shards. CI runs the same
  chain through the read-only half. The coupling base is resolved from the
  repository, never assumed to be `origin/main`.
- **B-3 (rules).** Three standing rules load at init (orchestrator,
  governed artifact reads, the coherence guard) and three path-scoped rules
  load on touch (`memory-invariants` for the admission, understanding,
  recall, and MCP crates; `build-commands` for anything under `crates/`,
  `apps/`, `eval/`, `docker/`, or `deploy/`;
  `derived-artifacts-are-compiler-output` for `.derived/**`). A scoped rule
  reinforces a standing one where the work is; it never replaces it.
- **B-4 (agents).** `architect`, `explorer`, `implementer`, and `reviewer`,
  all self-contained, with the read-only ones actually read-only.
- **B-5 (hooks).** `.claude/settings.json` reads and never writes, with one
  sanctioned exception: it recompiles the registry after a spec edit, when
  the session is live and can commit the result. It reports freshness at
  session start and at stop, checks staleness after a hashed-input edit,
  blocks `gh pr create` on a stale tree, uncommitted shards, or a red
  coupling gate without an inline waiver, and refuses a push that would
  update the default branch. Every hook acts on the repository the command
  targets, not on the session's project, and says so when it skips. The
  default branch and the binary are resolved, not assumed.
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
- **FR-004.** `make verify SPEC=<id>` runs every `verify:cli` line of that
  spec through `spec-spine verify` and reports the first failing command;
  `--plan` prints the commands and runs none of them.
- **FR-005.** The em-dash hook refuses a write that introduces U+2014.
- **FR-006.** `make gate` writes nothing. Running it leaves
  `git status --porcelain` unchanged, on a fresh tree and on a stale one.
- **FR-007.** The CLI refuses to run below the pin: `spec-spine.toml [meta]
  required_version` states the same floor `AGENTS.md`, `README.md`, and
  `govern.yml` state.

## 5. Acceptance criteria

- **AC-1.** `make ci` exits 0 on a clean checkout with `spec-spine` 0.18.0
  on `PATH`.
- **AC-2.** `scripts/spec-dag.sh` reports the corpus acyclic with every
  dependency lower-numbered.
- **AC-3.** `make gate` exits 0 on a clean checkout and leaves the working
  tree unchanged.

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
  pattern covered at all. `[index.slices]` carried the same defect and is
  fixed with it: `governance` held five dead globs, and `deployment` and
  `evaluation` held `deploy/**` and `eval/**`. Correcting them moves every
  index shard hash even though `deploy/` and `eval/` do not exist and the
  matched file set stays empty, but not because slice patterns are hashed:
  they are not. `spec-spine.toml` is itself a hashed input, folded whole
  into the global scalar every index shard carries, so any edit to that
  file restales every shard, a comment or a whitespace change included.
  Verified at 0.14.0 by appending only a comment, which exits `index
  check` 2. Both tables were almost certainly copied from spec-spine's
  own spec 012 example, which still teaches the dead form. Measured with
  the pinned 0.14.0 binary, which is
  what CI recomputes against: before the change, appending a line to
  `standards/spec/contract.md` left `index check` at exit 0; after it, the
  same edit exits 2. Regenerating rewrites all 30 index shards, which is
  the one-time cost of the patterns finally covering bytes and the
  evidence the hole was real rather than cosmetic. The `spec-spine` pin is
  untouched here, and B-2 is unaffected: `make ci` and CI still run the
  same targets. No spec text names the hashed-input patterns, so this is a
  choice the corpus was silent on and B-7's dated decision entry is the
  right instrument.

- **D-7 (2026-09-09, pin bump and the coverage amendment).** The
  `spec-spine` pin moves from 0.14.0 to 0.17.0 in every site that states
  it (`govern.yml`, `AGENTS.md`, `README.md`, the architect agent). As in
  D-4 the corpus was verified byte-compatible first: 0.17.0's `compile
  --check` and `index check` both report fresh against the shards on
  `main`, and no registry shard other than 000's and this spec's own
  changes here. The `L-008` warnings 0.17.0 reported were the dead globs
  D-6 already fixed, so `lint --fail-on-warn` is clean under the new pin
  without further change.

  One gate step could not be: `index coverage --fail-on-untraced` refuses
  an empty coverage universe (spec-spine 059) rather than passing it
  vacuously, and this corpus has no packages, so under 0.17.0 the step
  could only refuse. The flag was named in the bootstrap spec's section 9
  and in `standards/spec/contract.md`, neither of which is silent, so a
  decision entry could not remove it. The maintainer amended both on
  2026-09-09: coverage runs as a report until the first package carries
  source files, and as a refusal from then on, under the same `Cargo.toml`
  guard the cargo gates already use. `Makefile` and `govern.yml` apply the
  guard; the bare `index coverage` line stays in both so the empty universe
  is reported on every run rather than skipped. The flag comes back on its
  own the day spec 010 lands a workspace. B-2 is unchanged: `make ci` is
  still the definition of what CI validates, and every target is still
  guarded so the composite is green on a specify-only tree, which is
  exactly the property the unguarded flag had broken.

  What the bump buys: `verify` running a spec's declared acceptance (049),
  `index diagnostics` for unresolved units (050), `couple` naming the
  `extends` crossing that cleared a change (052), the `L-008` lint that
  found D-6's dead globs (057), `registry plan` answering blocked as well
  as ready (060), `[meta] required_version` so the CLI can check its own
  floor (062), and a malformed spec id refused rather than panicking (070).
  Setting `required_version` is the obvious follow-on and is its own
  change.

- **D-8 (2026-09-09, kit v18 and the amendment it forced).** The
  `spec-spine` kit moves to v18 and the pin to 0.18.0 in every site that
  states it (`govern.yml`, `AGENTS.md`, `README.md`, the architect agent,
  AC-1), plus `spec-spine.toml [meta] required_version = ">=0.18.0"`, which
  is D-7's named follow-on and is load-bearing now rather than cosmetic: the
  kit's PR gate calls `spec-spine check`, and on a binary below 0.18.0 that
  verb does not exist, so the gate refuses every `gh pr create` and names the
  binary that could not answer. Stating the floor in the config makes the CLI
  say so itself, before any verb runs.

  As in D-4 and D-7 the corpus was verified byte-compatible first, and this
  time the answer is more precise than "fresh". 0.18.0 moves the registry
  shard envelope from `specVersion` 1.1.0 to 1.2.0, so all 29 registry shards
  are rewritten; every `shardHash` in them is unchanged, and no codebase-index
  shard moves from the compile at all. The corpus content the ledger commits
  to is identical, and only the schema stamp on it moved. `lint
  --fail-on-warn` and `compile --check --fail-on-warn` are both clean under
  the new pin without further change.

  What the bump buys, and what forced the amendment. The kit is no longer the
  fifteen skills D-5 adopted: spec-spine's spec 081 cut it to ten, retiring
  `cleanup`, `implement-plan`, `refactor-claude-md`, `research` and
  `validate-and-fix`, none of which the loop ever called, and renaming
  `/init` to `/prime` so the verb the loop starts with is not the verb every
  other tool spells `init`. `scripts/verify-spec.sh` is deleted: spec-spine
  0.15.0 absorbed it into `spec-spine verify`, and a harness carrying a second
  implementation of one protocol is exactly the drift this repository refuses
  elsewhere. `spec-spine check` (spec 075) replaces the `compile --check` plus
  `index check` pair with one verb answering for both committed trees, and it
  keeps the never-writes contract both primitives had. The hooks are rewritten
  to act on the repository the command targets rather than the session's
  project, to resolve the binary and the default branch instead of assuming
  them (specs 051, 072), and to stop regenerating the index at session stop:
  a session that has ended cannot commit what it wrote, so the write left
  `.derived/` dirty and the next run refused to start on it. A fourth rule,
  `derived-artifacts-are-compiler-output`, arrives scoped to `.derived/**` as
  the worked example of the pattern; `governed-artifact-reads` stays
  unconditional, because the mistake it prevents has the shape of *not*
  touching that path.

  This is an amendment, not a decision entry, and the distinction matters.
  B-1, B-2, B-3, B-5, FR-004, AC-1, the `establishes` list and the summary all
  stated things that v18 changes, so none of them was silent and B-7's
  instrument could not reach them. D-5 anticipated exactly this and said so:
  porting the hooks "changes what B-5 requires, which is an amendment for a
  human to file, not a mid-build edit". The maintainer directed the v18
  adoption and approved the amendment on 2026-09-09; the agent authored the
  text and this entry records that authority rather than assuming it. Three
  clauses are additions rather than rewrites and would have been legitimate
  either way: FR-006 and AC-3 assert the read-only property `make gate` now
  has, and FR-007 asserts the version floor.

  Two pieces of the kit are deliberately not taken verbatim. The kit's own
  `Makefile` and `govern.yml` run `check --fail-on-unresolved` and an
  unguarded `index coverage --fail-on-untraced`; on this corpus both can only
  refuse. The first because a repository specified before it is built carries
  an unresolved unit for every pending spec, which is the state spec-spine 050
  made opt-in for precisely this case, and the second because spec-spine 059
  refuses an empty coverage universe, which is the refusal D-7 already
  guarded on `Cargo.toml`. So the kit's gate semantics are folded into this
  repository's targets rather than replacing them: `gate` and `refresh` are
  the kit's names and the kit's split, `spine`, `ci`, `attest`, `deny` and
  `spec-dag` stay, and `--fail-on-warn` is adopted on `check` because it is
  clean here today. Both flags come back on their own terms: coverage the day
  spec 010 lands a workspace, unresolved the day the corpus builds what it
  claims.

  The merge driver (`.githooks/`, and the stanza binding it to the shard
  globs in `.gitattributes`) is installed although the kit's own README says
  most adopters do not need it, this repository included: sharding already
  makes two pull requests touching different specs write disjoint files, and
  the loop is one spec per pull request. It is opt-in per clone and inert
  until `./.githooks/enable-merge-driver.sh` runs, it never substitutes for
  the staleness gate that proves a merge is what the corpus compiles to, and
  it is claimed here so the ownership ratchet holds it. `.gitattributes` joins
  `establishes` and the hashed inputs with it.

## Verification

```verify:cli
make gate
make spine
scripts/spec-dag.sh
```
