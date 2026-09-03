---
id: "042-portability-and-migration"
title: "Portability: a documented export anyone can read, and an import path off the system this replaces"
status: approved
kind: "feature"
domain: "ingest"
created: "2026-09-03"
authors: ["Bartek Kus"]
implementation: pending
risk: medium
wave: 4
depends_on:
  - "041-privacy-and-egress-boundary"
establishes:
  - "crates/aicortex-ingest/src/portable/mod.rs"
  - "crates/aicortex-ingest/src/portable/export.rs"
  - "crates/aicortex-ingest/src/portable/import.rs"
  - "crates/aicortex-ingest/src/portable/format.md"
  - "crates/aicortex-ingest/src/sources/openbrain.rs"
  - "crates/aicortex-ingest/tests/portable.rs"
extends:
  - { spec: "030-source-adapter-framework", unit: "crates/aicortex-ingest/src/registry.rs", nature: additive }
  - { spec: "020-http-api-and-scopes", unit: "crates/aicortex-api/src/router.rs", nature: additive }
summary: >
  A memory store that cannot be left is a hostage situation, and a product
  whose pitch is ownership has to prove it. Export writes a documented,
  self-describing archive of a scope: memories, provenance, entities, edges,
  lifecycle history, and optionally embeddings, in a format specified here
  and readable without this software. Import reads it back. The same
  machinery carries the migration path off the system this product replaces,
  as an ordinary source adapter reading a user's own data at their request.
---

# 042: Portability

## 1. Purpose

Two audiences need this. A user deciding whether to trust this product with
ten years of history needs to see the exit before the entrance. And a user
of the predecessor needs a way across, which is also the most credible
argument this product can make to the people best positioned to evaluate
it.

## 2. Territory

The portable format, its reader and writer, and the migration adapter,
inside `aicortex-ingest`.

## 3. Behavior

- **B-1 (the format).** A directory or tar archive with a versioned
  `manifest.json` (format version, produced-by, scope descriptor, counts,
  content digest), newline-delimited JSON files for memories, provenance,
  entities, edges, mentions, and lifecycle events, and an optional
  `embeddings/` directory of raw vectors with their model descriptor. The
  format is specified in `format.md` in enough detail to be implemented by
  someone with no access to this code.
- **B-2 (self-describing and stable).** Every record carries its schema
  version. The format is additive within a major version; a reader ignores
  unknown fields and refuses an unknown major version with a clear message.
- **B-3 (export).** `POST /scopes/{id}/export` runs as a job with progress
  on the event stream, streams the archive to the caller or to configured
  object storage, and never includes another scope's data. Embeddings are
  optional because they are large and reproducible.
- **B-4 (import).** Import is a source adapter, so it inherits dedup,
  provenance, resumability, and the gate. Memories arrive with their
  original timestamps and actors, and provenance records both the original
  source and the import.
- **B-5 (round trip).** Export then import into an empty scope reproduces
  the memory set, the graph, and the lifecycle states exactly, with new
  local ids and a mapping table recorded in the import run.
- **B-6 (the migration adapter).** `openbrain.rs` reads a user's own data
  from a predecessor deployment they control, over its public HTTP surface,
  with credentials the user supplies for their own instance. It maps rows
  to candidates: content, timestamps, and whatever metadata is present,
  with anything unrecognized preserved verbatim in provenance rather than
  discarded. Embeddings are not carried across, because the model differs;
  memories re-embed locally on arrival.
- **B-7 (clean room).** The adapter is written from the observed public
  HTTP surface, not from that system's source, per 000 §6. Its fixtures are
  recorded responses, and a comment at the top of the module states this.
- **B-8 (trust on migration).** Imported memories arrive as `Assertion` by
  an `Import` actor and never as `Instruction`, regardless of what they
  were. Promotion is a human act in this system (023).
- **B-9 (no lock-in in the other direction).** Export is available to any
  principal with `memory.read` on the scope, is not rate limited beyond the
  general limits, and requires no support request.

## 4. Functional requirements

- **FR-001.** Export then import into an empty scope reproduces counts,
  bodies, actors, timestamps, graph structure, and lifecycle states.
- **FR-002.** A third-party reader written against `format.md` alone
  parses the archive, asserted by a test that uses only the documented
  fields.
- **FR-003.** An archive with a bumped major version is refused with a
  clear message; one with an unknown minor field imports.
- **FR-004.** The migration adapter maps the recorded fixture responses to
  the expected memories, preserving unrecognized metadata in provenance.
- **FR-005.** Migrated memories are `Assertion` by `Import` and are
  re-embedded under the active local model.
- **FR-006.** An export contains no data from another scope, asserted over
  a two-scope fixture.

## 5. Acceptance criteria

- **AC-1.** `cargo test -p aicortex-ingest --locked --test portable`
  passes.
- **AC-2.** A real migration of at least 1000 rows from a predecessor
  instance completes, with the counts recorded in the spec's Status note.

## 6. Out of scope

Importing from competing commercial memory products, which is a later
adapter if there is demand. Continuous synchronization with another store,
which would create two authorities.

## 7. Resolved decisions

- **D-1 (2026-09-03, this spec).** Embeddings are optional in the export
  and are not carried in the migration. They are large, model-specific, and
  cheaply regenerated locally; carrying them would tie an archive to one
  model forever.

## Verification

```verify:cli
cargo test -p aicortex-ingest --locked --test portable
```
