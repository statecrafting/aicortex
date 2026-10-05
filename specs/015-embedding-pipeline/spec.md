---
id: "015-embedding-pipeline"
title: "Embeddings: a local default, a governed provider trait, an outbox worker that cannot lose a memory"
status: approved
kind: "kernel"
domain: "memory"
created: "2026-09-03"
authors: ["Bartek Kus"]
implementation: in-progress
risk: critical
wave: 1
depends_on:
  - "014-memory-lifecycle-and-erasure"
establishes:
  - "crates/aicortex-embed/Cargo.toml"
  - "crates/aicortex-embed/src/lib.rs"
  - "crates/aicortex-embed/src/provider.rs"
  - "crates/aicortex-embed/src/local.rs"
  - "crates/aicortex-embed/src/remote.rs"
  - "crates/aicortex-embed/src/chunk.rs"
  - "crates/aicortex-embed/src/worker.rs"
  - "crates/aicortex-embed/src/registry.rs"
  - "crates/aicortex-embed/src/migrations.rs"
  - "crates/aicortex-embed/src/static_model.rs"
  - "crates/aicortex-embed/src/config.rs"
  - "crates/aicortex-embed/src/activation.rs"
  - "crates/aicortex-embed/src/service.rs"
  - "crates/aicortex-embed/src/operator.rs"
  - "apps/aicortex/src/embedding.rs"
  - "crates/aicortex-embed/src/wordpiece.rs"
  - "crates/aicortex-embed/tests/worker.rs"
  - "crates/aicortex-embed/tests/chunk.rs"
  - "crates/aicortex-embed/tests/static_model.rs"
  - "crates/aicortex-embed/tests/config.rs"
  - "crates/aicortex-embed/testdata/vectors/"
  - "crates/aicortex-store/src/embedding_memory.rs"
  - "crates/aicortex-store/tests/embedding_staging.rs"
extends:
  - { spec: "012-store-schema-and-repositories", unit: "crates/aicortex-store/src/migrations.rs", nature: additive }
  - { spec: "012-store-schema-and-repositories", unit: "crates/aicortex-store/src/lifecycle.rs", nature: additive }
  - { spec: "012-store-schema-and-repositories", unit: "crates/aicortex-store/src/memory_repo.rs", nature: additive }
  - { spec: "012-store-schema-and-repositories", unit: "crates/aicortex-store/src/lib.rs", nature: additive }
  - { spec: "010-chassis-adoption-and-workspace", unit: "apps/aicortex/manifest.toml", nature: additive }
  - { spec: "012-store-schema-and-repositories", unit: "crates/aicortex-store/Cargo.toml", nature: additive }
  - { spec: "014-memory-lifecycle-and-erasure", unit: "crates/aicortex-store/src/erasure.rs", nature: additive }
  - { spec: "012-store-schema-and-repositories", unit: "crates/aicortex-store/tests/common/mod.rs", nature: additive }
  - { spec: "014-memory-lifecycle-and-erasure", unit: "crates/aicortex-store/tests/lifecycle.rs", nature: additive }
  - { spec: "012-store-schema-and-repositories", unit: "crates/aicortex-store/tests/repo.rs", nature: additive }
  - { spec: "012-store-schema-and-repositories", unit: "crates/aicortex-store/tests/schema.rs", nature: additive }
  - { spec: "014-memory-lifecycle-and-erasure", unit: "crates/aicortex-store/tests/erasure.rs", nature: additive }
  - { spec: "013-write-gate-and-redaction", unit: "crates/aicortex-gate/tests/common/mod.rs", nature: additive }
  - { spec: "013-write-gate-and-redaction", unit: "crates/aicortex-gate/Cargo.toml", nature: additive }
  - { spec: "013-write-gate-and-redaction", unit: "crates/aicortex-gate/src/lib.rs", nature: additive }
  - { spec: "013-write-gate-and-redaction", unit: "crates/aicortex-gate/tests/capture.rs", nature: additive }
  - { spec: "013-write-gate-and-redaction", unit: "crates/aicortex-gate/tests/compile_fail/", nature: additive }
  - { spec: "010-chassis-adoption-and-workspace", unit: "apps/aicortex/Cargo.toml", nature: additive }
  - { spec: "010-chassis-adoption-and-workspace", unit: "apps/aicortex/src/main.rs", nature: additive }
  - { spec: "010-chassis-adoption-and-workspace", unit: "apps/aicortex/src/cell.rs", nature: additive }
  - { spec: "012-store-schema-and-repositories", unit: "apps/aicortex/tests/migrate.rs", nature: additive }
  - { spec: "053-host-library-mode", unit: { kind: crate, id: "aicortex-external-host-fixture" }, nature: additive }
  - { spec: "010-chassis-adoption-and-workspace", unit: { kind: section, file: "Cargo.toml", anchor: "workspace.dependencies" }, nature: additive }
constrains:
  - { flavor: invariant-freeze, unit: "crates/aicortex-embed/src/worker.rs", note: "a memory is never left unembedded and unreported; constitution XI" }
summary: >
  The predecessor captured a memory in two non-atomic calls: insert the row,
  then update the embedding. A failure between them produced a memory that
  search could never reach and nothing ever retried. This spec makes the
  second half a durable job: capture stages outbox work in the same
  transaction as the row, a worker drains it idempotently with backoff and a
  dead letter state that preflight reports, and every stored vector names
  the model, dimension, and normalization it was produced under so a model
  change is a migration rather than a silent reinterpretation.
---

# 015: Embeddings

## 1. Purpose

Two defects of the predecessor meet here. The first is the orphaned
embedding (`openbrain://orphaned-embedding`): a row that exists but cannot
be found, produced by a two-call capture with no retry. The second is the
unversioned vector (`openbrain://unversioned-embeddings`): a fixed
dimension hard-coded in ten SQL files and a model name in forty-two source
files, so changing either meant re-embedding blind.

A third concern is new. If this product's claim is that the user owns their
memory, then sending every captured sentence to a third-party embedding API
by default contradicts the claim. Local inference is the default; remote
providers exist, are opt-in, and are visible in the manifest ceiling.

## 2. Territory

The `aicortex-embed` crate and the `embedding`, `chunk`, and
`embedding_model` tables. Extends 012's migration list and 010's manifest,
which gains its first egress entry only when a remote provider is enabled.

## 3. Behavior

- **B-1 (staged in the write).** Capture stages an outbox row naming the
  memory id and the active model revision, in the same rahi `txn` as the
  memory (012 B-4). There is no code path that inserts a memory without
  staging its embedding work.
- **B-2 (the worker).** A single worker per node drains the outbox under a
  rahi lease with its fencing token, in batches, and is idempotent per
  `(memory_id, model_revision)`: a repeated delivery overwrites rather than
  duplicating. Failures retry with exponential backoff and jitter to a
  configured attempt ceiling, then move to `DeadLetter` with the error
  recorded.
- **B-3 (dead letters are loud).** `preflight` reports the dead-letter
  count and the oldest pending age, `/metrics` exposes both, and a
  non-zero dead-letter count is a readiness warning. A memory that cannot
  be embedded is a visible defect, never a silent one.
- **B-4 (the trait).** `trait EmbeddingProvider { fn id(&self) ->
  ModelId; fn dims(&self) -> u16; fn normalized(&self) -> bool; async fn
  embed(&self, batch: &[&str]) -> Result<Vec<Vector>>; }`. Providers are
  selected by configuration and resolved once at boot.
- **B-5 (local is the default).** `LocalProvider` runs a bundled
  sentence-embedding model in-process on CPU, with the weights fetched to
  `models/` at first boot by `preflight` from a configured URL and verified
  against a pinned digest, or supplied by the image. With the local
  provider selected the process makes no outbound call at all, and the
  manifest declares no egress host, which is what makes the privacy claim
  provable (041).
- **B-6 (remote is governed).** `RemoteProvider` calls a configured
  endpoint through the kernel's `Governed<Egress>` facade
  (`rahi://015`). Enabling it requires adding the host to the manifest
  ceiling, which is a reviewable diff. A remote provider that is
  configured but absent from the ceiling fails at boot, not at first use.
- **B-7 (chunking).** Bodies above the chunk threshold are split into
  overlapping chunks on sentence boundaries with a configured target and
  overlap. Each chunk carries its ordinal and byte range, and each is
  embedded separately. A memory's score is the best chunk's score, and the
  matching chunk is reported in the recall trace so a citation points at
  the sentence, not the document.
- **B-8 (the model registry).** `embedding_model` records every model
  revision ever used: `model_id`, `dims`, `normalized`, `revision`,
  `first_seen`, `active`. `embedding` rows reference it. A query embedding
  is compared only against rows of the same revision (constitution XII),
  enforced by a predicate, not by convention.
- **B-9 (re-embedding is a migration).** Changing the active model does not
  invalidate old vectors. It marks a new revision active and enqueues a
  re-embed pass over the scope, which runs in bounded batches under a
  lease; both revisions are queryable during the transition and the old
  revision is dropped by an explicit operator verb once coverage is
  complete. `preflight` reports coverage per revision.
- **B-10 (vector form).** A `Vector` is `f32` little-endian, L2-normalized
  when the provider says so, stored as a BLOB through rahi's binary values
  (`rahi://016`), with `dims` recorded beside it. Storage never guesses the
  layout from the byte length.

## 4. Functional requirements

- **FR-001.** A capture followed by an induced worker crash before commit
  leaves the outbox row present; the next drain embeds it exactly once.
- **FR-002.** Two concurrent workers on one outbox produce one embedding
  row per `(memory, revision)`, asserted under rahi's lease.
- **FR-003.** A provider that always errors moves the row to
  `DeadLetter` after the configured attempts, and `preflight` reports a
  non-zero count.
- **FR-004.** With the local provider selected, a test asserts the process
  opens no socket during a capture, and the manifest contains no egress
  host.
- **FR-005.** A remote provider configured but absent from the manifest
  ceiling fails `preflight` with a named capability error.
- **FR-006.** A query embedded under revision 2 never matches rows stored
  under revision 1, asserted by a fixture holding both.
- **FR-007.** Chunking a 40 KiB body produces chunks whose ranges cover the
  body with the configured overlap and reassemble to the original.

## 5. Acceptance criteria

- **AC-1.** `cargo test -p aicortex-embed --locked` passes.
- **AC-2.** `aicortex preflight` reports the active model revision, the
  pending and dead-letter counts, and coverage per revision.

## 6. Out of scope

Similarity search itself (016), ranking (018), and semantic
deduplication (034). The choice of a specific local model, which is
configuration with a pinned digest rather than a spec-level commitment.

## 7. Resolved decisions

- **D-1 (2026-09-03, this spec).** Local inference is the default and the
  weights are fetched by `preflight` rather than baked into the image. The
  image stays small and multi-arch, and the fetch is a verified,
  one-time, reportable step rather than a hidden runtime download.
- **D-2 (2026-09-03, this spec).** A memory's score is its best chunk's
  score rather than a pooled document vector. Pooling dilutes a long note
  until nothing in it matches, which is the failure users describe as the
  system forgetting things it was told.
- **D-3 (2026-09-26, implementation).** The durable-work statement carries the
  model identity selected through the leader, and an in-transaction guard
  aborts staging if activation changes or the named revision is absent before
  commit. Capture does not call this primitive until the application owns
  provider configuration, initial activation, and the managed worker
  lifecycle. Existing capture therefore remains available while the spec is
  in progress.
- **D-4 (2026-09-28, implementation).** Each model revision has its own Rahi
  processor identity. Activating a new revision therefore cannot make its
  worker claim old-revision work, and an old worker can drain its partition to
  a terminal no-op without dead-lettering memories after a model switch.
- **D-5 (2026-09-28, implementation).** Re-embedding leases serialize normal
  scheduler passes, while the durable processing identity supplies correctness
  if an expired holder overlaps its successor. Rahi 0.4.0 does not expose a
  fenced transaction that accepts the scheduler's insert statements, so both
  holders may attempt the same staging operation and the unique processing key
  reduces it to one durable job.
- **D-6 (2026-09-28, implementation).** Chunk boundary preference recognizes
  ASCII sentence terminators and newlines. UTF-8 byte ranges remain exact and
  all scripts are covered without splitting a code point, while language-aware
  segmentation and its CJK corpus remain owned by spec 040.
- **D-7 (2026-09-28, implementation).** Model revisions increase
  monotonically. A rollback is a new revision with the former artifact rather
  than reactivating an older number, which preserves the meaning of durable
  work and stored-vector identities.
- **D-8 (2026-09-28, implementation).** The unreleased migration 9 creates the
  derivative tables with the chunk identifier, model foreign key, and
  vector-length constraints in their final shape. No repair migration or
  table rebuild is needed before the first release of this schema.
- **D-9 (2026-09-29, implementation).** Quarantined content is not eligible
  for embedding. Its staged durable job moves directly to the chassis dead
  letter so the same processing identity remains available to `Work::requeue`;
  spec 023's governed admission must activate the memory and requeue that
  identity in one transaction. Re-embedding and coverage exclude it until
  then. This keeps content with unestablished origin out of both local
  derivatives and remote provider calls without making later admission a
  silent no-op against a terminal `done` row.
- **D-10 (2026-09-29, implementation).** Queue health is deployment-wide and
  includes unfinished work for inactive revisions. Activating a replacement
  revision must not hide the prior partition: its revision-bound worker drains
  those jobs to a terminal no-op, and preflight remains loud until that drain
  occurs.
- **D-11 (2026-09-29, implementation).** Migration 9 is non-additive because
  an older binary cannot erase the chunk, vector, or embedding-queue rows it
  does not know about. Rollback across this schema version must therefore be
  refused.
- **D-12 (2026-09-29, implementation).** A failed per-item derivative commit
  consumes the durable work retry budget unless it is a claim conflict. This
  keeps deterministic store-limit failures from blocking the rest of a batch.
- **D-13 (2026-09-29, implementation).** Worker reports count quarantined work
  separately from exhausted failures, matching queue health and readiness.
- **D-14 (2026-09-29, implementation).** One worker item refuses raw vector
  payloads above 1 MiB before opening the derivative transaction. This leaves
  headroom below the pinned engine's 2 MiB WAL ceiling, so the queue can record
  a retry instead of submitting an entry the store cannot accept.
- **D-15 (2026-09-29, implementation).** A model deactivation that races an
  already-held claim atomically records the deferred attempt and requeues the
  same processing identity through Rahi's public work API. The row does not
  wait for claim expiry or become an observable dead letter, so temporary
  deactivation cannot consume the configured provider-failure budget. The
  worker offsets recorded deactivation and quarantine attempts when applying
  that budget after either identity is requeued.
- **D-16 (2026-09-29, implementation).** Embedding erasure now stages Rahi
  receipt and processing-row deletion, and the worker uses Rahi's coordination
  tables. The standalone cell must therefore declare the pinned chassis's
  `rahi.receipts` and `rahi.coordination` migration sets. This extends the
  migration seam without moving product migrations out of
  `Aicortex::migrations()`: spec 010 FR-003's application-list assertion is
  still governed by spec 012 D-7, while spec 012 D-8 owns the binary migration
  test changed here. The coordination SQL intentionally appears in both the
  legacy application list and the named chassis set; it is byte-identical and
  idempotent, and the test asserts that compatibility until a governed
  migration removes the legacy copy.
- **D-17 (2026-09-29, implementation).** The embedding pipeline reaches the
  memory table only through the storage crate's scoped adapter. Worker input,
  re-embedding selection, lifecycle guards, coverage, and queue classification
  therefore remain inside spec 012's SQL scope-predicate ratchet. Live totals
  come from the maintained scope counters rather than scanning memory rows.
- **D-18 (2026-09-29, implementation).** Product preflight queue health is
  scope-bound across every model revision. This narrows D-10's deployment-wide
  wording to preserve the storage isolation invariant: an operator-wide metric
  requires a separately authorized surface and cannot be inferred by a scoped
  preflight read.
- **D-19 (2026-09-29, implementation).** Each worker drain runs the chassis
  work sweep for the embedding namespace before claiming new work. The sweep
  closes expired attempts without applying its namespace-global terminal
  ceiling; after reclaim, the worker applies the row-specific ceiling that
  excludes recorded deactivation and quarantine attempts, and dead-letters an
  exhausted claim before calling the provider. This covers failures that
  killed or hung a prior worker without letting an administrative deferral
  consume the provider-failure budget.
- **D-20 (2026-10-03, implementation).** D-3 defers capture staging until the
  application owns provider configuration, initial activation, and the worker
  lifecycle; it is silent on host-library mode (053), where the host owns all
  three. Removing the capture call from `MemoryRepo::insert` (02d282a, merged
  in #41) therefore left host captures without embedding work even when the
  host had activated a model. A host now captures through
  `MemoryRepo::insert_with_embedding`, which takes the `ActiveEmbedding` the
  host read through the leader and stages the memory and its durable job,
  under the same processing identity as `stage_embedding`, in the host-owned
  transaction (053 B-9). `MemoryRepo::insert` is unchanged and remains the
  standalone capture primitive under D-3 until the application wiring lands.
  Whether 053 B-10's host staging surface should name the new seam instead of
  `MemoryRepo::insert` is left to the owner.
- **D-21 (2026-10-04, implementation).** Every memory write stages its
  embedding work, superseding D-3's deferral of capture staging and D-20's
  separate host seam: `MemoryRepo::insert_with_embedding` is removed.
  `MemoryRepo::insert`, the host-library path of spec 053, and
  `Lifecycle::capture` take an `EmbeddingTarget`: the caller's leader read of
  the active model. With a model active, the write stages that revision's job
  under D-3's activation guard. With none active, it stages no job and a guard
  aborts the write if a model became active before commit. A memory therefore
  never commits beside an active model without its job, capture stays
  available before first activation, and the re-embedding pass that follows
  activation (B-9) covers memories written while no model was active.
- **D-22 (2026-10-05, implementation).** The concrete `LocalEngine` runs a
  static sentence-embedding model (the Model2Vec family): a WordPiece
  tokenizer plus a token embedding table whose mean is the sentence vector,
  optionally L2-normalized. The weights are one pinned safetensors file with a
  single `embeddings` tensor, and the tokenizer is a second pinned artifact,
  both verified through `WeightArtifact`. A transformer runtime was weighed
  and refused: the candle stack pulls `paste` (RUSTSEC-2024-0436,
  unmaintained), which `deny.toml` rejects and which this spec may not
  waive, and `tokenizers` pulls it too, so the WordPiece subset the family
  needs (`BertNormalizer`, `BertPreTokenizer`, `WordPiece`) is implemented
  here and anything else in a `tokenizer.json` is a configuration error. The
  normalizer follows BERT exactly: after NFD it drops Unicode category Mn,
  splits on ASCII symbols and category P (not category S), and drops control
  and format characters, using the `unicode-general-category` tables. All
  float arithmetic is `ndarray`'s: the module contains no float operator, so
  the workspace `float_arithmetic` ratchet takes no exception. Tokens outside
  the vocabulary are dropped from the mean, and a text with no embeddable
  token is a validation error that follows the normal retry and dead-letter
  path. Real-model verification is an ignored test keyed to
  `AICORTEX_TEST_MODEL_DIR`; no test touches the network.
- **D-23 (2026-10-05, implementation).** Provider configuration is read from
  `AICORTEX_EMBED_*` variables through rahi's `EnvReader` and resolved once by
  `EmbeddingConfig::boot`. With no provider selected and no model named the
  configuration is `Disabled`, not an error: the deployment captures with no
  embedding work until a model is configured (D-21), and B-5's "local is the
  default" governs which provider a named model uses, not whether a model
  must exist. The service name that holds embedding egress grants is
  `embedding`. `EmbeddingConfig::check` answers the B-5 and B-6 ceiling
  question from the manifest alone (`Manifest::covers`), opens no socket, and
  returns a `CapabilityFailure` naming the check (`embedding.remote.egress`
  or `embedding.weights.egress`), the service, and the host; it converts to
  `Error::Denied`. It is a plain function so a chassis preflight extension
  can mount it unchanged; until one exists the application cannot make
  `preflight` call it.
- **D-24 (2026-10-05, implementation).** Activation is `activate_provider`:
  the first provider becomes revision 1; an identical identity keeps the
  active revision so a restart is a no-op; any other identity becomes the
  latest recorded revision plus one (D-7). Activation does not enqueue the
  re-embedding pass, because that pass is bound to one scope (D-17, D-18) and
  the pipeline holds no scope inventory outside the storage crate's scoped
  adapter; the per-scope pass stays the explicit `stage_reembedding_batch`
  step an operator or a scope owner's surface drives.
- **D-25 (2026-10-05, implementation).** The worker lifecycle is
  `run_worker(worker, store, settings, shutdown)`: a future that drains, idles,
  retries store errors after a backoff, and returns a `ServiceReport` when the
  caller's `shutdown` future resolves. It spawns nothing and holds no handle,
  so a managed-service host mounts and joins it and no untracked task can
  outlive its owner. A drain in progress at shutdown may finish for a bounded
  grace; past it the drain is dropped, which is the induced-crash case the
  durable queue and its fenced claims already make safe (FR-001).
- **D-26 (2026-10-05, implementation).** The B-8 revision predicate is
  `ModelRegistry::vectors`: the caller passes the `ModelRevision` its query
  embedding was produced under, and the statement matches revision, model
  identity, width, and normalization together. A vector of another revision
  is not returned, so it cannot be compared; spec 016's recall must read
  vectors only through it.
- **D-27 (2026-10-05, implementation).** The operator verbs of B-9 are routes
  on the cell's operator surface, not new argv verbs: spec 010 B-3 gives the
  chassis every verb and the `Cell` trait offers no verb hook, while
  `operator_routes` is mounted behind the operator role. They are
  `GET /operator/embedding/status`, `POST .../activate`, `.../reembed`, and
  `.../drop`, each naming its scope explicitly (D-24). The provider
  configuration is read from the process environment when the router is
  built; a malformed configuration is held and reported by each route,
  because a `Cell` cannot fail while building routes. Activation of a remote
  provider checks the ceiling and then reports that this build links no remote
  transport, rather than activating a model nothing can call. Queue gauges in
  the chassis `/metrics` registry are not wired: D-18 makes queue health
  scope-bound, and a collector would have to read every scope, which needs the
  separately authorized operator-wide surface D-18 names.

## Status (2026-10-05, in progress: chassis hooks required)

Implemented and locally verified: the provider contracts, bounded chunking,
the monotonic model registry, the revision-partitioned durable worker, erasure
integration and capture staging, the re-embedding scheduler, artifact
verification, the governed remote boundary, the preflight report, and now the
runtime that was missing. Provider configuration is read from the environment
and checked against the manifest ceiling as a plain function (D-23). A concrete
in-process engine backs `LocalProvider` (D-22). Activation numbers revisions
monotonically (D-24). The worker is a service function a host mounts and joins
(D-25). The B-8 revision predicate is `ModelRegistry::vectors` (D-26). The
operator verbs (status, activate, re-embed, drop) are operator routes (D-27),
and one test drives them through the real engine from first activation to a
model change and a drop.

The spec is not complete. What the pinned chassis (rahi 0.4.0) blocks:

- **Worker lifecycle (B-2).** `aicortex serve` does not run the worker. Rahi has
  no managed-service lifecycle (rahi spec 047, planned for 0.5.0), and an
  untracked task would outlive its owner. `run_worker` is ready to mount.
- **Product preflight (B-3, AC-2, FR-003, FR-005 at the process boundary).**
  The chassis has no product preflight extension hook (rahi spec 049, planned
  for 0.5.0), so `aicortex preflight` cannot call `EmbeddingConfig::check` or
  print `EmbeddingPreflight`. The check and the report exist as functions, and
  `GET /operator/embedding/status` serves the report meanwhile. A remote
  provider missing from the ceiling is refused by `check`, by
  `activate_configured`, and by the activate route, but it does not yet fail
  `serve` at boot.
- **Metrics (B-3).** The registry hook exists (`rahi_edge::obs::current`), but
  queue health is scope-bound (D-18), so a collector would need the
  operator-wide surface D-18 names.
- **First-boot model fetch (B-5, D-1).** Fetching missing weights belongs to
  preflight and needs a fetch transport; activation requires the artifacts to
  be present (supplied by the image or the operator). No remote HTTP transport
  is linked, so `RemoteProvider` is reachable only through a host-supplied
  transport.

Requirement status: FR-002 and FR-007 have direct tests. FR-001 has an induced
crash test (a worker task aborted inside inference leaves the outbox row, and
a later drain embeds it once). FR-003 has direct dead-letter transition and
library report coverage, but its application preflight path stays open with
the hook. FR-004 has a test that the shipped manifest declares no egress and
that booting the local provider and embedding opens no socket; the probe
brackets the embedding path and does not cover a booted `serve`, so the
whole-process form stays open. FR-005 has a test that the check names the
failure; the `preflight` and boot forms stay open with the hook. FR-006 has a
fixture holding two revisions that the revision predicate keeps apart; spec
016's recall must read through that predicate.

Every memory write, standalone capture and host insert alike, stages embedding
work whenever a model is active (D-21). A fresh deployment captures without
jobs until an operator activates a model and runs the re-embedding pass for
each scope, which covers memories written while no model was active.

## Verification

```verify:cli
cargo test -p aicortex-embed --locked
```
