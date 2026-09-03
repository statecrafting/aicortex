---
id: "000-aicortex-bootstrap"
title: "Bootstrap spec system for aicortex (specify first, build by spec, clean of the system it replaces)"
status: approved
kind: "constitutional-bootstrap"
domain: "governance"
created: "2026-09-03"
authors: ["Bartek Kus"]
implementation: n-a
risk: critical
wave: 1
origin:
  retroactive: true   # authority held since before the graph existed
unamendable:
  - "markdown-truth-boundary"
  - "json-truth-boundary"
  - "determinism-requirement"
  - "directory-name-equals-id"
  - "clean-room-rule"
summary: >
  The founding spec. It fixes the authoring boundary (markdown in,
  compiler-owned JSON out), the identity rule (directory name equals id),
  the typed authority graph, the closed taxonomies, the lifecycle that makes
  a spec a work order, and the gate chain. It also fixes the two rules that
  are specific to this product: the coherence guard that forbids amending a
  spec to ratify code, and the clean-room rule that separates what may be
  carried from the system this product replaces (observations) from what may
  not (its expression).
---

# 000: Bootstrap

## 1. The authoring and derived boundary

Authored truth is markdown with YAML frontmatter under `specs/` and
`standards/`. Machine truth is JSON under `.derived/`, emitted only by
`spec-spine` and read only through its subcommands. Both shard trees are
committed so that a reviewer sees the same registry the gate sees; neither
is hand-edited, and `build-meta.json` is gitignored because wall-clock
metadata is not truth.

No `jq`, `python`, `awk`, or `sed` ever reads `.derived/`. A typed read
fails at the deserializer with a clean error when the schema moves; an
ad-hoc read encodes a stale assumption and keeps working.

## 2. Identity

One spec per directory, `specs/NNN-slug/spec.md`, where the directory name
equals the frontmatter `id` and `NNN` is a unique three-digit ordinal. The
ordinal is the build order. Ordinals are never reused and never renumbered:
a retired spec keeps its number, because references to it exist in commits,
in the ledger, and in other specs.

## 3. The typed authority graph

Eight edges: `establishes`, `extends`, `refines`, `supersedes`, `amends`,
`co_authority`, `constrains`, `references`. All but `references` carry
authority. A spec claims territory with `establishes`, reaches into another
spec's territory with `extends`, and freezes a property across territory it
does not own with `constrains`. Authority units are `file` (bare string;
trailing slash means subtree), `section`, `symbol`, `directory`, `crate`,
and `module`.

`require_ownership` is on from the first commit. Every source file is
claimed by exactly one spec through an owning edge, and a changed file that
no spec claims is a `C-002` refusal at pull request time. This is cheap to
hold in a greenfield corpus and impossible to retrofit later.

## 4. Closed taxonomies

`domain` is the product responsibility a spec serves and `kind` is its
role; both are closed enums in `spec-spine.toml`, and a value outside them
is a compile error. `wave` is the build-order wave, 1 to 4. The domains are
`governance`, `chassis`, `memory`, `retrieval`, `protocol`, `clients`,
`ingest`, and `ops`. The kinds add `constraint` to the usual set: a
constraint spec owns no code and exists to freeze a property across other
specs' units, which is how a privacy or safety boundary is made checkable
rather than aspirational.

## 5. Determinism

Compilation is a pure function of configuration and file contents, so the
same tree yields byte-identical shards on any machine, staleness is a
content-hash comparison, and the registry is diffable in review.

## 6. The clean-room rule

This product replaces a system published under a licence that forbids
commercial derivative works. This repository is Apache-2.0 and is intended
to be sold, so the separation must be explicit and permanent:

- No source, SQL, schema definition, prompt text, or prose is copied from
  that system into this repository, in whole or in paraphrase.
- What may be carried is observation: how the deployed system behaved,
  which failures it produced, which capabilities its users valued. Every
  such borrowing is cited `openbrain://<topic>` so it is visible in the
  registry and auditable in review.
- Reading a user's own data out of that system, at that user's request, is
  interoperability and is permitted (spec 042). The request shape is
  derived from the public HTTP surface as observed.
- A session that wants to open that system's source to answer a design
  question stops and specifies from the problem instead.

`docs/design/00-lineage.md` carries the observation table. A pull request
that adds an `openbrain://` citation adds it there too.

## 7. The coherence guard

When the coupling gate fails because code and its owning spec disagree, the
resolution is never to edit the spec so the code passes. Surface the
contradiction and let a human decide. A spec is amended when the design
changed and the amendment says so; it is never amended to ratify what a
session happened to write. The escape valve is a scoped
`Spec-Drift-Waiver:` line in the pull request body with human approval,
which a driven session never grants itself.

## 8. Lifecycle as scheduling

| status + implementation | meaning |
|---|---|
| `draft` | proposed, not ratified; never scheduled |
| `approved` + `pending` | a work order |
| `approved` + `in-progress` | being built |
| `approved` + `complete` | built and verified |
| `approved` + `n-a` | a record that owns no code |
| `superseded` / `retired` | history, ordinal retained |

Approval is a human act. A machine-authored spec is born `draft`, and a
spec can be pulled back to `draft` at any time to hold it, which the
orchestrator reports as a blocker rather than scheduling around.

## 9. The gate chain

`make spine` runs `compile`, `index`, `lint --fail-on-warn`, `index check`,
`couple --base origin/main`, and `scripts/spec-dag.sh`. `make ci` adds
`index coverage --fail-on-untraced` and, once `Cargo.toml` exists, `cargo
build`, `test`, `clippy -D warnings`, `fmt --check`, and `deny`. CI runs
the same set with `compile --check` in place of `compile`. Every ordinary
spec ends with a `## Verification` section whose `verify:cli` blocks are
commands with exit codes, run after merge.

## 10. Bootstrap order

000 fixes the system. 001 establishes the harness that drives it. 002
states the thesis and the wave plan. Everything else is a work order
against that plan.
