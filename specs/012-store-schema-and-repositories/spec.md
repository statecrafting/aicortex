---
id: "012-store-schema-and-repositories"
title: "Schema and repositories: versioned migrations, scope in the predicate, counters instead of scans"
status: approved
kind: "kernel"
domain: "memory"
created: "2026-09-03"
authors: ["Bartek Kus"]
implementation: pending
risk: critical
wave: 1
depends_on:
  - "011-memory-model"
establishes:
  - "crates/aicortex-store/Cargo.toml"
  - "crates/aicortex-store/src/lib.rs"
  - "crates/aicortex-store/src/migrations.rs"
  - "crates/aicortex-store/src/memory_repo.rs"
  - "crates/aicortex-store/src/provenance_repo.rs"
  - "crates/aicortex-store/src/scope_repo.rs"
  - "crates/aicortex-store/src/counters.rs"
  - "crates/aicortex-store/src/cursor.rs"
  - "crates/aicortex-store/tests/schema.rs"
  - "crates/aicortex-store/tests/repo.rs"
extends:
  - { spec: "010-chassis-adoption-and-workspace", unit: "apps/aicortex/src/cell.rs", nature: additive }
  - { spec: "010-chassis-adoption-and-workspace", unit: { kind: section, file: "Cargo.toml", anchor: "workspace.dependencies" }, nature: additive }
constrains:
  - { flavor: invariant-freeze, unit: "crates/aicortex-store/src/memory_repo.rs", note: "every statement carries a scope predicate; no post-filtering" }
summary: >
  The tables and the only code allowed to touch them. Schema arrives as an
  ordered list of rahi migrations with versions, never as SQL a user pastes
  from a guide. Every repository call takes a scope and puts it in the SQL,
  so an unscoped read is not expressible. Aggregates are maintained
  counters updated in the writing transaction, because the predecessor's
  statistics call read every row's metadata into memory. Listing is
  keyset-paged; there is no offset pagination and no unbounded select.
---

# 012: Schema and repositories

## 1. Purpose

Two of the predecessor's defects were properties of its persistence layer
rather than of its ideas. It had no migrations at all: the schema was SQL
pasted from a document, so every deployment was a different shape and every
later extension had to guess the prior state (`openbrain://no-schema-management`).
And its statistics endpoint loaded every row's metadata to count things
(`openbrain://stats-loads-everything`). Both are cheap to prevent at the
start and expensive to fix once data exists.

The third defect was structural: functions ran as the database service role
and bypassed row security entirely, so isolation depended on every call
site remembering to filter (`openbrain://service-role-everywhere`). This
spec answers that with a repository API in which the scope is a parameter of
every call, not a condition someone might append.

## 2. Territory

The `aicortex-store` crate: the migration list, the repositories, the
counters, and the cursor. It is the only crate that writes SQL against the
memory tables; later specs that add tables (embeddings, entities, edges,
ingest state) own their own migrations and repositories and declare an
`extends` edge on `migrations.rs` to register them in order.

## 3. Behavior

- **B-1 (migrations).** `migrations() -> &'static [rahi_store::Migration]`,
  each with a monotonic `version`, a stable `name`, and idempotent SQL.
  They are run by rahi's `migrate` verb, never at boot; `serve` refuses to
  start behind the schema and says which command to run (`rahi://030`).
  A migration is never edited after it ships: a mistake is a new
  migration.
- **B-2 (tables).** `memory` (the record of 011, one row, body inline),
  `provenance` (one row per memory, with `derived_from` as a child table
  `memory_derivation`), `scope` (owner subject, kind, key, created), and
  `scope_counter` (per scope, per kind, per status). `memory` carries
  `scope_id`, `status`, `kind`, `trust`, `created`, `updated`, a
  `fingerprint` (see 014), and `schema_version`. Indexes: `(scope_id,
  status, created desc)`, `(scope_id, fingerprint)` unique among non-erased
  rows, and `(status, updated)` for the curators.
- **B-3 (scope in the predicate).** Every repository method takes a
  `&Scope` and emits `WHERE scope_id = ?` in the statement. There is no
  method that returns memories across scopes, and no method that takes a
  raw SQL fragment from a caller. A test greps the crate for a `SELECT`
  against `memory` without `scope_id` in the same statement and fails on a
  match.
- **B-4 (write path).** `MemoryRepo::insert(txn, &Memory, &Provenance,
  &Outbox work)` appends every statement to one rahi `TxnBuilder`, so the
  memory, its provenance, its derivation rows, its counter updates, and its
  outbox row commit together or not at all (constitution XI). The
  repository never opens its own transaction and never calls `execute`
  directly on a path that also stages outbox work.
- **B-5 (read consistency).** Reads that decide admission or uniqueness use
  `query_consistent`; list and detail reads use `query` on the local
  replica. Every call site states which and why in a comment, per rahi's
  store discipline.
- **B-6 (counters, not scans).** `scope_counter` is updated in the same
  transaction as the row that changes it. `stats(scope)` is a bounded read
  of that table. No aggregate in this system is computed by reading bodies,
  and a test asserts `stats` issues exactly one statement whose plan
  touches only the counter table.
- **B-7 (keyset paging).** `list(scope, filter, Cursor)` orders by
  `(created desc, id desc)` and pages by the last seen pair. `Cursor` is an
  opaque, signed, base64url string carrying the pair and the filter hash,
  so a cursor cannot be replayed against a different filter or scope.
  Offset paging does not exist.
- **B-8 (body size).** A body larger than the configured ceiling (default
  64 KiB of text) is rejected by the gate (013) before reaching here;
  the repository asserts the ceiling again and returns
  `Error::Validation`, because a second check at the storage boundary costs
  nothing and catches a future caller that bypassed the gate.
- **B-9 (no cross-scope join).** Scopes are isolated in SQL. A future
  sharing feature is a spec that adds an explicit grant table and its own
  predicate; it is not a relaxation of B-3.

## 4. Functional requirements

- **FR-001.** Against rahi's single-voter test harness: `migrate` from
  empty applies every migration in order and reports the final version;
  running it twice is a no-op.
- **FR-002.** An insert of a memory, its provenance, two derivation rows,
  a counter update, and an outbox row commits atomically; a forced failure
  on the last statement leaves no trace of the first.
- **FR-003.** A read for scope A never returns a row of scope B, asserted
  over a fixture with two scopes and identical content.
- **FR-004.** `list` over 5000 rows returns every row exactly once across
  pages, with no duplicates at page boundaries when a concurrent insert
  lands.
- **FR-005.** A cursor from filter X is rejected when presented with
  filter Y.
- **FR-006.** `stats` cost does not grow with the number of memories,
  asserted by statement count and by plan inspection.
- **FR-007.** The B-3 grep test fails when an unscoped `SELECT` is added.

## 5. Acceptance criteria

- **AC-1.** `cargo test -p aicortex-store --locked` passes.
- **AC-2.** `aicortex migrate` on an empty data directory reports the
  expected schema version, and `aicortex serve` against a stale directory
  refuses with the rahi stale exit code.

## 6. Out of scope

Admission rules (013), lifecycle transitions (014), and every table owned
by a later spec: embeddings (015), the vector index (016), entities and
edges (017), ingest state (030), review queue (023).

## 7. Resolved decisions

- **D-1 (2026-09-03, this spec).** Bodies live inline in the `memory` row
  rather than in a separate table. The ceiling is small, the row is almost
  always read with its body, and hiqlite replicates statements, so a
  second table would double the statement count on the hot path without
  reducing the replicated bytes.

## Verification

```verify:cli
cargo test -p aicortex-store --locked
```
