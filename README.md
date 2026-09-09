# aicortex

Persistent, governed memory for AI clients. One store, one protocol, any
client: Claude Code, Codex, Cursor, Antigravity, and anything else that
speaks MCP over streamable HTTP.

aicortex is a governed cell built on [rahi](../rahi): rauthy for identity,
hiqlite for state, a hash-chained decision ledger, a deny-by-default kernel,
one container, one volume, one origin. It adds the product: a memory record
with provenance and a trust class, a transactional capture path, embeddings
that name their model, entities and typed edges, hybrid recall with an
explainable trace, curation that keeps the store from rotting, and an
evaluation corpus that says whether any of it got better.

It is a rewrite in intent of an earlier system, and it exists because that
system's shape could not be fixed in place: one static shared key for every
user, a service role that bypassed row security, a schema that lived inside
a prompt string, a capture path that could leave a memory permanently
unfindable, and a feature ecosystem that grew by forking the server. Those
are cited throughout the corpus as `openbrain://` observations. No code,
schema, or prose was taken from it; see spec 000 §6.

## Status

Specified, not yet built. The corpus under `specs/` is the whole design.
Every ordinary spec is `approved` with `implementation: pending`, spec
ordinals are the build order, and code arrives one spec per session under
`crates/` and `apps/`. Before spec 010 lands there is no `Cargo.toml`, and
every Makefile target is guarded for that.

Read `specs/002-memory-thesis/spec.md` first: it holds the nine
responsibilities, the crate topology, and the four waves.

## The gate

```sh
make gate       # read-only: check --fail-on-warn, lint --fail-on-warn, couple, spec-dag
make refresh    # writing:   compile, index
make spine      # refresh + gate
make ci         # spine + coverage (--fail-on-untraced once Cargo.toml exists) + the cargo gates
make verify SPEC=018-retrieval-and-recall-trace
```

Governance is `spec-spine` 0.18.0 on `PATH`; CI pins the same version, and
`spec-spine.toml [meta] required_version` makes the CLI refuse to run below
it. Derived shards are committed and read only through the `spec-spine` CLI.

## Working the backlog

`AGENTS.md` is the cross-agent protocol: how a session initializes, how it
picks the next spec, and what it must satisfy before it stops. The
orchestrator's state root for this project lives under `data/`, which is
gitignored. Any spec can be pulled back to `status: draft` to hold it for
human review; drafts are visible as blockers and are never scheduled.

## License

Apache-2.0, see [`LICENSE`](LICENSE). The chassis and the other products in
this family are Apache-2.0 as well, so a crate can move in either direction
across the family without a licence review.
