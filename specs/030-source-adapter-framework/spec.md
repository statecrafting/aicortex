---
id: "030-source-adapter-framework"
title: "Source adapters: one contract, incremental and resumable, so a capability is never a fork"
status: approved
kind: "kernel"
domain: "ingest"
created: "2026-09-03"
authors: ["Bartek Kus"]
implementation: in-progress
risk: high
wave: 3
depends_on:
  - "024-decision-and-audit-references"
establishes:
  - "crates/aicortex-ingest/Cargo.toml"
  - "crates/aicortex-ingest/src/lib.rs"
  - "crates/aicortex-ingest/src/adapter.rs"
  - "crates/aicortex-ingest/src/run.rs"
  - "crates/aicortex-ingest/src/drive.rs"
  - "crates/aicortex-ingest/src/routes.rs"
  - "crates/aicortex-ingest/src/preflight.rs"
  - "crates/aicortex-ingest/src/cursor.rs"
  - "crates/aicortex-ingest/src/mapping.rs"
  - "crates/aicortex-ingest/src/registry.rs"
  - "crates/aicortex-ingest/src/migrations.rs"
  - "crates/aicortex-store/src/ingest_tables.rs"
  - "apps/aicortex/src/ingest_surface.rs"
  - "apps/aicortex/src/ingest_preflight.rs"
  - "crates/aicortex-ingest/tests/common/mod.rs"
  - "crates/aicortex-ingest/tests/adapter.rs"
  - "crates/aicortex-ingest/tests/run.rs"
  - "crates/aicortex-ingest/tests/routes.rs"
  - "apps/aicortex/tests/ingest.rs"
extends:
  - { spec: "012-store-schema-and-repositories", unit: "crates/aicortex-store/src/migrations.rs", nature: additive }
  - { spec: "012-store-schema-and-repositories", unit: "crates/aicortex-store/src/lib.rs", nature: additive }
  - { spec: "014-memory-lifecycle-and-erasure", unit: "crates/aicortex-store/src/erasure.rs", nature: additive }
  - { spec: "010-chassis-adoption-and-workspace", unit: "apps/aicortex/src/cell.rs", nature: additive }
  - { spec: "010-chassis-adoption-and-workspace", unit: "apps/aicortex/tests/cell.rs", nature: additive }
  - { spec: "015-embedding-pipeline", unit: "apps/aicortex/tests/embedding_process.rs", nature: additive }
  - { spec: "015-embedding-pipeline", unit: "apps/aicortex/tests/embedding.rs", nature: additive }
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

- **D-2 (2026-11-26, build, crate layout).** `aicortex-ingest` holds the trait,
  the runner, the cursor, the mapping helpers, the registry, and the HTTP
  handlers of B-9. Spec 002 section 4 orders the layers as types, admission,
  understanding, recall, surfaces, intake, curation, proof, and the cell, and
  dependencies point downward only, so intake depends on `aicortex-api` (the
  scope gate, the errors, the caller and the event bus) and on the store, the
  gate and the record, and never on `aicortex-curate` or `aicortex-eval`. The
  routes reach the API router through its additive extension seam (020 D-20,
  023 D-2), so `router.rs` is not edited, and `apps/aicortex` composes them.
  `drive.rs` splits the page loop from the runner's vocabulary in `run.rs`;
  `routes.rs` and `preflight.rs` are the HTTP half and the preflight read, and
  `tests/common/mod.rs` is the shared fixture. The crate's manifest has only
  workspace dependencies and lists every one the later parts use, so no later
  part edits it; the root workspace entry and the app's edge are 010 D-16,
  which lands after the crate (D-11). `RunId` lives in `mapping.rs`, where the
  item mapping uses it, so the mapping and its tests land before the runner.
  `tokio` is taken with its `sync` and `time`
  features, which the pacing and the cancel flag of B-8 and FR-006 use, and no
  third-party package is added.
- **D-3 (2026-11-26, build, the trait as built).** B-1 sketches `async fn
  probe`, `async fn page` and `fn map` over a `cfg`. The registry holds
  adapters by name, so the trait has to be object safe, and an `async fn` in a
  trait is not: `probe` and `page` return boxed futures. `cfg` is an
  `AdapterContext`, which carries the run's options, the page limit the runner
  asks for, and the egress facade restricted to the hosts the adapter declared.
  B-6 and FR-004 need the declared hosts before any fetch, so the trait gains
  `hosts()`, empty by default. An adapter's error is `Transient` (retried up
  to the run's ceiling) or `Fatal`; its text is for the operator and is never
  stored, returned or put in a Decision, because it may quote the source. The
  adapter-level `Candidate` carries the kind, the title and the text and
  nothing about origin, so everything B-5 and B-7 derive is the runner's.
- **D-4 (2026-11-26, build, migration 16).** The tables are migration 16
  (`INGEST_VERSION`, additive, in `crates/aicortex-store/src/ingest_tables.rs`
  beside 023's and 024's), because the migration list is the store's and this
  crate depends on it. 024 took 15, so 16 is the next free version (053 D-9
  requires the list to be contiguous); whoever merges after another renumbers
  `INGEST_VERSION` and `EXPECTED_SCHEMA_VERSION` follows it. `ingest_run` is
  the run record (state, counters, a closed failure code, the principal),
  `ingest_cursor` is B-3's cursor (one per scope and source), and
  `ingest_item` is B-4's mapping, unique per scope, source and external id. The
  mapping carries a BLAKE3 digest of what the adapter mapped the item to, which
  is derived from content, so 014 B-7 reaches it, under an additive `extends`
  edge on 014's `erasure.rs`: an erasure deletes the mappings of the memories it
  tombstones in one statement, outside the counted spans, and not as a
  `DERIVATIVES` entry, because an entry costs one statement per memory per
  table and a scope's erasure batch is already at the log's write size (adding
  one overflows
  `fr005_a_scope_erasure_of_five_thousand_memories_is_bounded_and_resumable`).
  A scope's erasure deletes the scope's rows of all three tables. A re-import
  after an erasure is a new capture and does not revive the mapping. No table
  holds a body, a title, or the text of an error.
- **D-5 (2026-11-26, build, how a page commits).** One page is one
  transaction: the items, their mappings, the digest keys and references of
  what the gate held back, the run's counters, and the cursor commit together.
  The runner takes the lease of B-2 for each page, not once for the run, because
  a lease expires after ten seconds whether or not its holder is finished
  (`rahi_store::LEASE_TTL_SECONDS`), and opens the transaction with one guard
  statement written on the lease's fence row: a `CASE` that yields `NULL` into
  its `NOT NULL` column unless the token is still the newest, the run's row
  still says `running`, and the stored cursor is the one the page was fetched
  from (compared with `IS`, so an absent cursor matches an expected absence).
  An absent row and a changed row both abort the batch, which a predicate that
  matched no row would not. `fenced_txn` cannot be used because it fences
  `UPDATE` and `DELETE` only and a page inserts. Two items of one page that
  touch the same external id, content, or memory are staged in separate
  transactions (the second is read against a store the first has reached); the
  cursor moves only in the page's last, so a crash between them replays the
  page, and the mapping makes the replay an update. A failed page is retried
  from its first uncommitted item, and past the run's retry ceiling the run
  stops as `store_failed` with the cursor where it was. A run that reaches the
  last page clears its cursor in the same transaction, so the next run starts
  over (FR-001's re-run); a run that stops anywhere else keeps it, and the next
  run of that source and scope resumes there. What an adapter calls
  incremental is its own option, carried in the request.
- **D-6 (2026-11-26, build, Decisions).** A refusal and a quarantine are
  ledgered as the API's capture does (013 B-9): the runner stages the digest
  key, and for a quarantine the decision reference of 024 B-3, in the page's
  transaction, then appends the gate's Decision through `append_decision`
  after the commit, naming the person or client who started the run. The
  reason codes are the gate's, which 024 B-2 already closes, so no code is
  added. A Decision that cannot be appended stops the run with
  `ledger_unavailable` after that page, at the cursor the commit left, so the
  page is not replayed and the later pages wait for a chain that can take
  them. The Decisions of that one page are not retried: the memory is stored
  quarantined or not stored, which is the safe side, and the run's code says a
  gap exists. A run records no Decision of its own.
- **D-7 (2026-11-26, build, trust by kind).** B-7's three kinds map to the
  gate's origins and to a trust class: `Authenticated` is the registered
  adapter's origin and `Evidence`; `Export` is the same origin and
  `Assertion`; `Public` is the asserted origin the gate quarantines, and
  `Assertion`, unless `AICORTEX_INGEST_PUBLIC_ORIGIN=admit` gives it an export's
  path. The variable is read once by the cell, and a value other than
  `quarantine` or `admit` is a configuration error. No source produces
  instruction grade (019 B-4). The actor is `ActorKind::Import`, whose id is
  the run and whose label is the source; the starting subject is the run's
  `started_by` and the actor of every Decision.
- **D-8 (2026-11-26, build, runs).** A real run is a row, and at most one is
  `running` per scope and source (a partial unique index), so a second start is
  a conflict. A run that has not touched its row for 120 seconds is taken over:
  the new run ends it as `failed` with the error code `abandoned` (the row's
  state set stays closed) and inserts its own in one transaction, and
  the old one, if it is somehow alive, aborts on its next page's guard. Cancel
  sets a flag in the row, read before every page, so it reaches whichever node
  drives the run and takes effect within one page (FR-006). A dry run writes
  no row (B-10, FR-003): it lives on a bounded board in the driving node's
  memory, so it is visible there and gone with the process; it reads the saved
  cursor like a real run and keeps its own record of what it has seen, so a
  repeated item or repeated content counts as the real run will count it.
  Counts: `admitted` and `quarantined` are new rows, `updated` is an item
  whose external id was imported before, `merged` is a new external id whose
  content was already held (014 B-2), and every delivered item is in exactly
  one of those, `refused` or `unmapped`.
- **D-9 (2026-11-26, build, an update).** An item whose external id was
  imported before is captured again. The same content merges into the memory it
  already is; changed content is a new memory, and the older one is superseded
  by it in the same transaction (014 B-3) when it is still active. A
  quarantined replacement waits for review and leaves the older memory as it
  was. The mapping then names the newest memory.
- **D-10 (2026-11-26, build, egress).** Registration holds each declared host
  to the `http.egress` grants of the `ingest` service, through the manifest the
  kernel carries (`Governed::kernel().manifest()`), so an adapter whose host is
  absent from the ceiling is refused with `Error::Denied` before any fetch
  (FR-004); a declared host may not be a wildcard. An adapter's request needs a
  `Permit` from its context, which refuses an undeclared host and then asks the
  kernel. No concrete adapter exists in this spec, so the cell registers none
  and constructs no `Governed` facade for the `ingest` service; the first
  concrete adapter widens the manifest and constructs it, under its own
  spec's edge on the registry.
- **D-11 (2026-11-26, build, how the build lands).** This spec is built start
  to finish on one branch and lands in parts, as 017 D-8, 018 D-8, 025 D-7,
  026 D-13 and 023 D-10 did, in four pull requests in dependency order, each
  building on its own: the crate with the adapter trait, the cursor, the
  mapping, the registry, migration 16 and the erasure reach (a new crate whose
  manifest only inherits from the workspace is not a policy change under spec
  044 section 3.3); then the runner; then the root workspace entry and the
  app's edge alone (010 D-16, under policy isolation); then the run routes, the
  preflight check and the cell's wiring. The crate is a workspace member from
  the first part, through the root manifest's `crates/*` members glob, so its
  package enters `Cargo.lock` there; the third part adds only the
  `[workspace.dependencies]` entry other crates name it by. The spec stays
  `in-progress` until its
  last part lands, so no part claims it complete.
- **D-12 (2026-11-26, build, the cell's part).** `apps/aicortex` hands the run
  routes to the API's extension seam beside 023's, builds the runner over an
  empty registry (D-10), reads the intake policy from its environment (D-7),
  and refuses to start `serve` on a policy that names neither choice. AC-2's
  report is the `app.ingest` preflight check, which prints the registered
  sources with their kind and hosts, holds each host to the manifest ceiling
  again (a ceiling narrowed since registration fails with the chassis's
  `capability http.egress on <host> for service ingest` wording), and reads the
  last run of each source in each scope from the read-only store view; a failed
  last run is a warning, and a partial read says so. The check needs no
  manifest entry of its own and the cell adds no `[services.ingest]` to the
  manifest, because nothing here reaches the network.

## Status

- **2026-11-26.** `implementation: in-progress`. B-1 to B-10 and FR-001 to FR-006
  hold in `cargo test -p aicortex-ingest --locked` (the runner and the registry
  in `--test run` and `--test adapter`, the routes in `--test routes`), AC-1
  with them, and AC-2 in `cargo test -p aicortex --locked --test ingest` and
  the process-level `ac2_030_preflight_lists_the_registered_sources_and_their_runs`
  in `--test embedding_process`. No concrete adapter exists, so the registry is
  empty until 031.

## Verification

```verify:cli
cargo test -p aicortex-ingest --locked
```
