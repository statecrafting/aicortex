# AGENTS.md: aicortex

Cross-agent authority for aicortex, read by Claude Code, Codex CLI, Cursor,
Copilot, and claude-observatory's driven sessions via the AAIF/Linux
Foundation AGENTS.md standard. It is the single source for the session-init
protocol and the backlog discipline. Evolve the protocol by editing this
file, never the `/prime` skill that dispatches to it.

aicortex is persistent, governed memory for AI clients: a memory record with
provenance and a trust class, a transactional capture path, embeddings that
name their model, entities and typed edges, hybrid recall with an
explainable trace, curation, and an MCP surface every major client can
reach (the thesis is `specs/002-memory-thesis/spec.md`). It is a governed
cell on the **rahi** chassis, which it pins by version and never forks: rahi
owns identity, state, the decision chain, the kernel, the edge, packaging,
and the operational verbs. The repository is **specified before it is
built**: the corpus under `specs/` is the whole design, every ordinary spec
is `approved` and `implementation: pending`, and spec ordinals are the build
order. Code arrives one spec per session under `crates/`, `apps/`, `eval/`,
`docker/`, and `deploy/`.

Governance is `spec-spine` **0.18.0** on your `PATH` (CI pins the same
version, and `spec-spine.toml [meta] required_version` makes the CLI refuse
to run below it). All governed reads of `.derived/` go through its CLI.

## New Sessions

Run `/prime` as the first action of every new session. It reads this section
to derive its plan; anything added here is picked up on the next init.

> AGENTS.md is loaded implicitly as the protocol source, so `/prime` does not
> list it as a parallel read in step 1.

**Init protocol:**

0. **Load rules** (read first): `.claude/rules/orchestrator-rules.md`,
   `.claude/rules/governed-artifact-reads.md`,
   `.claude/rules/adversarial-prompt-refusal.md`. The path-scoped rules
   (`memory-invariants`, `build-commands`,
   `derived-artifacts-are-compiler-output`) load themselves when you touch
   their paths.

1. **Parallel reads.** Dispatch simultaneously (nothing here mutates the
   tree, so there is no ordering):
   - `CLAUDE.md`: what Claude Code needs beyond this file
   - `README.md`: project description and status
   - `standards/spec/contract.md`: the normative corpus contract
   - `standards/spec/constitution.md`: the fifteen principles, five frozen
   - `spec-spine --version`: the binary's version. Read this before believing any exit code below; a binary predating a flag a step passed makes that step's exit code meaningless
   - `spec-spine check`: freshness for **both** committed trees, the registry and the index, in one read (non-fatal; see below)
   - `spec-spine registry status-report --json --nonzero-only`: lifecycle counts
   - `spec-spine registry list --ids-only`: the spec inventory
   - `spec-spine registry plan`: the ready set (spec-spine 038): which specs can be worked on now and what blocks the rest; `/next` applies the approval and in-flight rules on top of it
   - `spec-spine index coverage`: which source files no spec claims (exit 2 if stale)
   - `scripts/spec-dag.sh`: the DAG is acyclic and every dependency is lower-numbered
   - `ls crates/ apps/ eval/ docker/ deploy/ 2>/dev/null`: what has been built so far (absent directories are expected before their spec lands)
   - `ls specs/ docs/design/`
   - `git log --oneline -10` and `git diff --stat HEAD~1`

2. **Emit** a `## primed: aicortex` block: the nine responsibilities
   in one line each with the crates that exist, the pinned rahi version, a `## lifecycle:` sub-section
   from the status report (approved/pending counts, and the next ready spec
   from `/next` if cheap), freshness verdicts, recent activity, and a
   ready-to-help line.

**Read discipline:** never parse `.derived/**/*.json` directly (no `jq`,
`python`, `awk`, `sed`); all structural and lifecycle data comes from
`spec-spine` subcommands.

**Freshness:** `spec-spine check` asks about both committed trees in one
call. It compiles and indexes in memory, compares against the committed
shards **without writing**, reports each tree on its own line
(`spec-registry:` and `codebase-index:`), and returns the more severe of the
two verdicts in the order `3`, `1`, `2`, `0`. It is non-fatal to `/prime`:
report and continue.

- **`0`:** both trees are exactly what the corpus compiles to. Report nothing.
- **`2` (stale):** read the report lines to say *which* tree moved (the
  composite code cannot), name the drifted shards, and report "run
  `spec-spine compile` and commit" or "run `spec-spine index`" accordingly.
  The lifecycle counts then come from the committed (stale) ledger; say so
  rather than presenting them as current.
- **`1`:** the corpus fails validation. Surface the violations and report the
  counts as unverified. This outranks `2`: staleness is not meaningful
  against a corpus that does not compile.
- **`3`:** the read was not performed. Treat freshness as unknown for both
  trees and report stderr verbatim. Most often a binary predating the verb,
  which is what the `--version` read above exists to tell you apart from
  drift. Never report "fresh" for a code you did not recognize.

Never substitute a plain `spec-spine compile` or `spec-spine index` here.
Writing repairs the tree as a side effect of reading it, which hides that the
*committed* copy was stale; `/prime` reports, it does not mutate, and `check`
carries the same never-writes contract.

**CLI missing:** if `spec-spine --version` fails, run `/setup`. Do not fall
back to ad-hoc parsing.

If any file is missing: log "not found" and continue.

## Working the backlog

This repo's backlog is its spec corpus. Every spec with `status: approved`
and `implementation: pending` is a work order. One session implements one
spec, start to finish, then stops. Specs `000`, `001`, and `002` are records
(`n-a` or `complete`), never work orders.

1. **Pick the spec.** The lowest-numbered spec with `implementation:
   pending` whose `status` is `approved` and whose `depends_on` are all
   `implementation: complete` or `n-a`. Use `/next`, or `spec-spine
   registry show <id> --json`; never guess. A `draft` spec is never picked:
   approval is a human act. If the spec's Territory section names an
   operator prerequisite (a rauthy binary, an S3 bucket, a
   published rahi version, an embedding model) that is missing, stop and
   report exactly what is needed instead of mocking around it.
2. **Branch and flip.** Work on a feature branch named after the spec id
   (`012-store-schema-and-repositories`). Flip the spec to `implementation: in-progress`,
   run `make refresh` (`spec-spine compile && spec-spine index`), and commit
   the flip with the regenerated `.derived/` shards before writing code.
   Never commit to `main`.
3. **Re-read the spec in full before coding.** The design truth precedes
   the code. If the design is imprecise, record the choice you make as a
   dated `D-n` entry under `## 7. Resolved decisions` (and drop a copy in
   `data/orchestrator/decision-dropbox/` when a driven session; the
   orchestrator seals it). If the design is *wrong*, stop and report the
   contradiction: never edit a spec afterwards to ratify what the code
   happened to do (`.claude/rules/adversarial-prompt-refusal.md`).
4. **Implement within the territory.** Every file you add under a crate
   must be claimed: add it to this spec's `establishes` list in the same
   change (the ownership ratchet, `C-002`, refuses an unclaimed source
   file). When you add a third-party dependency, add it to the workspace
   manifest's `[workspace.dependencies]` and declare the `extends` edge on
   spec 010's `Cargo.toml` section. Touching a file another spec owns
   requires an `extends` edge on that spec's unit. Do not edit `.derived/`
   by hand.
5. **Hold the memory invariants.** `.claude/rules/memory-invariants.md` is
   the checklist: capture is one transaction, every memory carries
   provenance and a trust class, every vector names its model, recalled
   content is delimited and labelled untrusted, erasure reaches the
   embeddings, and the chassis is consumed rather than reimplemented. A
   change that needs one of these relaxed is a human decision: stop and
   report.
6. **Run the gate before every commit.** `make spine`, then `make ci`. All
   must exit 0. Commit the regenerated `.derived/` shards with the code they
   describe.

   `make spine` is `make refresh` followed by `make gate`:

   ```sh
   spec-spine compile                                    # refresh: writes
   spec-spine index                                      # refresh: writes
   spec-spine check --fail-on-warn                       # gate: read-only
   spec-spine lint --fail-on-warn
   spec-spine couple --base "$BASE" --head HEAD
   scripts/spec-dag.sh
   ```

   `make ci` adds coverage as a report and, once `Cargo.toml` exists,
   coverage `--fail-on-untraced`, `cargo build`, `test`, `clippy -D
   warnings`, `fmt --check`, and `deny`.

   `BASE` is resolved from this repository rather than assumed to be
   `origin/main`: `$SPEC_SPINE_DEFAULT_BRANCH`, then `git symbolic-ref
   --short refs/remotes/origin/HEAD`, then `main`. The Makefile and the push
   gate resolve it the same way, so set that variable to override both.

   `check` is deliberately called without `--fail-on-unresolved`: this corpus
   is specified before it is built, so a pending spec legitimately carries
   unresolved units until its session lands. The flag comes back the day that
   stops being true. This list and the `govern.yml` job are kept identical:
   the skills tell their reader to run "the gate as `AGENTS.md` lists it", so
   a step CI enforces and this list omits is a step every session skips.
7. **Satisfy Acceptance criteria verbatim.** Run the spec's `##
   Verification` block locally with `/verify <id>`, which wraps `spec-spine
   verify <id>`, the same verb an orchestrator's verify stage runs after
   merge. Read the plan first with `spec-spine verify <id> --plan` when the
   spec is not one this session authored. If a criterion cannot be
   satisfied (external state, a missing sibling), keep `implementation:
   in-progress`, add a dated Status note to the spec saying exactly what
   remains, and report it. Flip to `implementation: complete` only when
   acceptance holds; recompile and commit.
8. **Ship.** `/ship` (gate, review, conventional commit naming the spec id
   such as `feat(011): ...`, push the feature branch, open the PR). The
   PR body is Summary plus Testing; no AI attribution, no session links.
   A `Spec-Drift-Waiver:` line needs explicit human approval; a driven
   session never self-approves one. Then stop: the next session takes the
   next spec.

## Available Agents

Agents live in `.claude/agents/`, all self-contained:

- `architect`: plans and decomposes against the corpus. Read-only.
- `explorer`: searches, traces dependencies, gathers context. Read-only.
- `implementer`: executes focused changes from a plan. Minimal diffs.
- `reviewer`: post-change review for bugs, correctness, spec drift, and the
  memory invariants. Read-only.

## Available Commands

Skills live in `.claude/skills/`.

The governed loop, in the order "Working the backlog" runs it:

- `/prime`: this protocol.
- `/setup`: install spec-spine and the Rust toolchain; verify the loop.
- `/next`: the next ready spec from `registry plan`, minus drafts, with in-flight specs and honest blockers.
- `/build <id>`: one spec start to finish per "Working the backlog".
- `/verify <id>`: run a spec's declared acceptance through `spec-spine verify <id>`.
- `/ship`: gate, review, commit on a feature branch, open a PR.
- `/shepherd`: watch the PR's checks, remediate red runs, merge when green,
  confirm the merge on disk.
- `/spec`: author a new spec from the template; next ordinal; DAG check.

The skills the loop calls:

- `/commit`: conventional commit, impact-focused, spec id in scope.
- `/code-review`: correctness, spec-drift, and memory-invariant review.

The ten are the spec-spine kit's, byte for byte (spec-spine spec 081). The
project layer the skills read lives in this file (the pin, the binary, the
gate command list in "Working the backlog", the resolved default branch) and
in the path-scoped rules (the memory invariants, the evaluation corpus); do
not edit a skill to add a project fact, add it here.

## Conventions

- Rust 2024, toolchain pinned in `rust-toolchain.toml` (spec 010); always
  `--locked`; `unsafe` is denied workspace-wide and allowed only in a
  named FFI block with a `// SAFETY:` comment; clippy `-D warnings`.
- Crates depend downward only (thesis §4), and every one of them depends on
  rahi rather than reimplementing it. A pull request that adds an HTTP
  server, a second store, a credential type, or a second hash chain is
  wrong by construction.
- Responsibility is `domain`, role is `kind`, build wave is `wave`; all
  three are in every spec's frontmatter and validated on compile.
- `data/` is the orchestrator's state root for this project; never commit
  it. `.derived/` shards are committed; `build-meta.json` is not.
- No em dash anywhere in authored text (a hook enforces file writes).
- Conventional commits, spec id as scope; no AI attribution; no session
  links in anything that lands in git or on GitHub.
- Derived artifacts are read only through `spec-spine` subcommands.
