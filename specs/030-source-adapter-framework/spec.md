---
id: "030-source-adapter-framework"
title: "Source adapters: one contract, incremental and resumable, so a capability is never a fork"
status: approved
kind: "kernel"
domain: "ingest"
created: "2026-09-03"
authors: ["Bartek Kus"]
implementation: pending
risk: high
wave: 3
depends_on:
  - "024-decision-and-audit-references"
establishes:
  - "crates/aicortex-ingest/Cargo.toml"
  - "crates/aicortex-ingest/src/lib.rs"
  - "crates/aicortex-ingest/src/adapter.rs"
  - "crates/aicortex-ingest/src/run.rs"
  - "crates/aicortex-ingest/src/cursor.rs"
  - "crates/aicortex-ingest/src/mapping.rs"
  - "crates/aicortex-ingest/src/registry.rs"
  - "crates/aicortex-ingest/src/migrations.rs"
  - "crates/aicortex-ingest/tests/adapter.rs"
  - "crates/aicortex-ingest/tests/run.rs"
extends:
  - { spec: "012-store-schema-and-repositories", unit: "crates/aicortex-store/src/migrations.rs", nature: additive }
  - { spec: "020-http-api-and-scopes", unit: "crates/aicortex-api/src/router.rs", nature: additive }
summary: >
  The predecessor's ecosystem grew by forking the server: fifty-odd
  capabilities, each a copy of the same six hundred lines, so a fix in one
  never reached the others. This spec is the alternative. A source is an
  implementation of one trait, registered by name, driven by one runner that
  owns fetching pages, mapping to candidates, passing the gate, staging the
  transaction, recording a resumable cursor, and reporting progress. Adding
  a source is a module and a fixture set, and it inherits dedup, retries,
  provenance, egress governance, and the untrusted content boundary without
  restating any of them.
---

# 030: Source adapters

## 1. Purpose

Most of what users valued in the predecessor was intake: importers from
every chat product, read-later services, note vaults, and mail
(`openbrain://valued-capabilities`). Every one of them was a separate
deployment of a separate copy of the server
(`openbrain://fork-per-feature`). The capability was real; the packaging was
the defect.

## 2. Territory

The `aicortex-ingest` crate: the trait, the runner, the cursor, the mapping
helpers, and the registry. Concrete adapters live in later specs and add
modules under this crate, each declaring an `extends` edge on the registry.

## 3. Behavior

- **B-1 (the trait).** `trait SourceAdapter { fn id(&self) -> SourceId; fn
  kind(&self) -> SourceKind; async fn probe(&self, cfg) -> Result<Probe>;
  async fn page(&self, cfg, cursor: Option<Cursor>) -> Result<Page>; fn
  map(&self, item: &RawItem) -> Result<Candidate>; }`. `Page` carries items
  and the next cursor. An adapter never writes to the store, never calls
  the gate, and never opens a transaction: it fetches and maps.
- **B-2 (the runner).** One runner drives every adapter: it acquires a rahi
  lease per `(scope, source)`, pages, maps, evaluates each candidate
  through the gate, stages admitted candidates with their outbox work in
  bounded transactions, advances the cursor in the same transaction as the
  items it covers, and emits progress events (020 B-6).
- **B-3 (resumable by construction).** Because the cursor commits with its
  items, an interrupted run resumes exactly where it stopped, with no
  duplicates and no gap. A run that cannot make progress after the retry
  ceiling stops with a recorded error and leaves the cursor where it was.
- **B-4 (idempotent by external id).** `ingest_item(source_id, scope_id,
  external_id, memory_id, digest)` is unique per source and external id.
  Re-running an import updates rather than duplicating, and the
  fingerprint merge of 014 B-2 catches the case where two sources carry the
  same content.
- **B-5 (provenance is automatic).** The runner constructs `Provenance`
  from the source, the external id, the item's own timestamp, and the run
  id. An adapter cannot omit it and cannot forge an actor: the actor is
  derived from the source registration and the authenticated principal who
  started the run.
- **B-6 (egress is governed).** Every network call an adapter makes goes
  through the kernel's governed egress facade, so an adapter's hosts appear
  in the manifest ceiling and a new adapter's network reach is a reviewable
  diff (`rahi://015`).
- **B-7 (trust by source kind).** `SourceKind` decides the default verdict
  path: `Authenticated` (the user's own account, verified) admits;
  `Export` (a file the user uploaded) admits with `Assertion`; `Public`
  (a fetched page, a shared channel) quarantines unless the deployment
  configures otherwise. The gate makes the final call (013 B-7).
- **B-8 (rate and budget).** Each run declares a rate limit and a total
  item budget, both configurable, both enforced by the runner rather than
  by each adapter.
- **B-9 (routes).** `POST /sources/{id}/runs` starts a run,
  `GET /sources/{id}/runs/{run}` reports it, `DELETE` cancels it. Progress
  streams on `/events`.
- **B-10 (dry run).** Every run can be a dry run: it maps and gates
  without writing, and reports what it would have admitted, merged,
  quarantined, and refused. A user importing ten years of chat history
  gets to see the shape before committing to it.

## 4. Functional requirements

- **FR-001.** A stub adapter over a fixture of 1000 items imports exactly
  1000 memories; re-running imports zero and updates 1000.
- **FR-002.** An induced crash mid-run resumes with no duplicate and no
  skipped item, asserted by content comparison against the fixture.
- **FR-003.** A dry run writes nothing and reports counts matching the
  subsequent real run.
- **FR-004.** An adapter whose host is absent from the manifest ceiling
  fails at registration, not at first fetch.
- **FR-005.** A `Public` source's items land quarantined by default.
- **FR-006.** Cancelling a run stops it within one page and leaves a
  consistent cursor.

## 5. Acceptance criteria

- **AC-1.** `cargo test -p aicortex-ingest --locked` passes.
- **AC-2.** `aicortex preflight` lists registered sources, their hosts, and
  the last run state per scope.

## 6. Out of scope

Every concrete adapter (031, 032, 033). Scheduling of recurring runs, which
is deployment configuration invoking the same routes.

## 7. Resolved decisions

- **D-1 (2026-09-03, this spec).** The adapter maps and never writes. It
  is the single rule that keeps a source from growing its own storage
  logic, which is how the predecessor's forks began.

## Verification

```verify:cli
cargo test -p aicortex-ingest --locked
```
