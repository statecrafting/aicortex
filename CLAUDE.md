# CLAUDE.md

Read `AGENTS.md` first: it carries the session protocol (`## New Sessions`)
and the backlog discipline (`## Working the backlog`). This file only holds
what Claude Code needs beyond it.

## What this is

aicortex is persistent, governed memory for AI clients. One store, one MCP
surface, any client. It is a governed cell on the **rahi** chassis: rahi
supplies identity (rauthy, same origin, bearer tokens for non-browser
clients), replicated state (hiqlite, in-process), a hash-chained decision
ledger, a deny-by-default kernel, an axum edge with probes and streaming,
and single-container packaging with preflight, migrate, backup, and restore.
aicortex supplies the product above it. Read
`specs/002-memory-thesis/spec.md` for the nine responsibilities, the crate
topology, and the four waves; `docs/design/00-lineage.md` for what was
carried from which prior system and why.

The repository is specified before it is built. Every ordinary spec is
`approved` plus `implementation: pending`, spec ordinals are the build
order, and code lands one spec per session. Before spec 010 lands there is
no `Cargo.toml`; every Makefile target and CI step is guarded for that.

## Commands

```sh
make gate       # read-only: spec-spine check --fail-on-warn, lint --fail-on-warn, couple, spec-dag
make refresh    # writing:   spec-spine compile, index
make spine      # make refresh + make gate
make ci         # make spine + index coverage --fail-on-untraced + the cargo gates
make build      # cargo build --workspace --locked
make test       # cargo test  --workspace --locked
make lint       # cargo clippy --workspace --all-targets --locked -- -D warnings
make fmt        # cargo fmt --all --check
make deny       # cargo deny check (when deny.toml exists)
make coverage   # spec-spine index coverage
make attest     # spec-spine attest --with-coupling -> .derived/attestation/
make verify SPEC=<id>         # spec-spine verify <id>: the spec's declared acceptance
spec-spine verify <id> --plan # print those commands and run none of them
scripts/spec-dag.sh           # depends_on is acyclic and only points to lower-numbered specs

# One crate, one test:
cargo test -p aicortex-recall --locked --test fusion
```

Exit codes of `spec-spine`: `0` ok, `1` validation failure or drift, `2`
stale, `3` I/O, parse, schema, or config. Since 0.18.0 a usage error is `3`,
not `2`, so `2` means staleness and nothing else. The `aicortex` binary
adopts rahi's four (`rahi://010`).

## Architecture in one screen

| Responsibility | Crates | Founding specs |
|---|---|---|
| chassis and the cell | `apps/aicortex`, `aicortex-types` | 010, 011 |
| admission | `aicortex-store`, `aicortex-gate` | 012, 013, 014 |
| understanding | `aicortex-embed`, `aicortex-index`, `aicortex-graph` | 015, 016, 017 |
| recall | `aicortex-recall` | 018, 019 |
| surfaces | `aicortex-api`, `aicortex-mcp` | 020, 021, 022 |
| curation and review | `aicortex-curate` | 023, 024, 034 |
| intake | `aicortex-ingest` | 030, 031, 032, 033, 035 |
| proof | `aicortex-eval`, `eval/`, `docker/`, `deploy/` | 040, 041, 042, 043, 044 |

Dependencies point downward only: types, then store and gate, then embed,
index and graph, then recall, then api and mcp, then ingest, curate and
eval. `apps/aicortex` composes them and implements rahi's `Cell`; nothing
depends on the app.

## Invariants that shape every change

- **The chassis is consumed, never forked.** No second HTTP server, store,
  identity path, credential type, or hash chain lives here. When rahi is
  wrong, fix rahi.
- **The subject is the only identity.** Scopes are owned by a rauthy `sub`;
  this repository mints no credential and accepts no static shared key.
- **Provenance or it is not stored.** Source, actor, time, trust class.
- **Recalled content is data, never instruction.** Delimited, labelled,
  framed as untrusted on every path that returns it to a model.
- **Capture is one transaction.** The row and its outbox work commit
  together; embedding and extraction are driven afterwards with retries and
  a dead letter state.
- **Every vector names its model.** Model, dimension, normalization. A
  query only compares against its own model's vectors.
- **Forgetting is real.** Erasure reaches content, chunks, embeddings, and
  index entries. Memory content never enters the decision chain.
- **Retrieval quality is measured.** A change to chunking, embedding,
  ranking, or fusion runs the evaluation corpus and reports the delta.
- **Egress is declared.** Every outbound call goes through the kernel's
  governed facade, within the manifest ceiling.

## Governance mechanics

- Every source file inside a crate must be specifically claimed by a spec
  (`require_ownership` is on). Add new files to the implementing spec's
  `establishes` in the same change.
- `.derived/` shards are committed; regenerate with `make refresh`
  (`spec-spine compile && spec-spine index`) and commit them with the
  change. `make gate` never writes: a gate that repairs the tree hides the
  defect it exists to find.
- Derived artifacts are read only through `spec-spine` subcommands.
- Hooks in `.claude/settings.json` recompile after spec edits, check
  staleness after hashed-input edits, block `gh pr create` on a red
  coupling gate, and block `git push` to `main`.
- The coherence guard: never edit an owning spec to make the gate pass on
  code that contradicts it. Surface the contradiction.

## House style

- No em dash character anywhere (chat, code, comments, specs, commits).
- Conventional commits with the spec id as scope: `feat(012): ...`.
- No AI attribution and no session links in commits, PR bodies, or comments.
- Specs follow `standards/spec/templates/spec-template.md`: Purpose,
  Territory, Behavior (B-n), Functional requirements (FR-nnn), Acceptance
  criteria (AC-n), Out of scope, Resolved decisions (D-n), `## Verification`.
- Clean room: nothing is copied from the prior system's source, schema, or
  prose. Cite its observed behavior with `openbrain://`, never its code.
