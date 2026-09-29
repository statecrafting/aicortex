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
  - "crates/aicortex-embed/tests/worker.rs"
  - "crates/aicortex-embed/tests/chunk.rs"
  - "crates/aicortex-embed/testdata/vectors/"
extends:
  - { spec: "012-store-schema-and-repositories", unit: "crates/aicortex-store/src/migrations.rs", nature: additive }
  - { spec: "010-chassis-adoption-and-workspace", unit: "apps/aicortex/manifest.toml", nature: additive }
  - { spec: "012-store-schema-and-repositories", unit: "crates/aicortex-store/Cargo.toml", nature: additive }
  - { spec: "012-store-schema-and-repositories", unit: "crates/aicortex-store/src/memory_repo.rs", nature: additive }
  - { spec: "012-store-schema-and-repositories", unit: "crates/aicortex-store/src/lib.rs", nature: additive }
  - { spec: "014-memory-lifecycle-and-erasure", unit: "crates/aicortex-store/src/erasure.rs", nature: additive }
  - { spec: "012-store-schema-and-repositories", unit: "crates/aicortex-store/tests/common/mod.rs", nature: additive }
  - { spec: "014-memory-lifecycle-and-erasure", unit: "crates/aicortex-store/tests/lifecycle.rs", nature: additive }
  - { spec: "012-store-schema-and-repositories", unit: "crates/aicortex-store/tests/repo.rs", nature: additive }
  - { spec: "014-memory-lifecycle-and-erasure", unit: "crates/aicortex-store/tests/erasure.rs", nature: additive }
  - { spec: "013-write-gate-and-redaction", unit: "crates/aicortex-gate/tests/common/mod.rs", nature: additive }
  - { spec: "013-write-gate-and-redaction", unit: "crates/aicortex-gate/Cargo.toml", nature: additive }
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
## Status (2026-09-29, in progress: runtime and chassis hooks required)

The provider contracts, bounded chunking, monotonic model registry,
revision-partitioned durable worker, erasure integration and capture-staging
primitive,
re-embedding scheduler, artifact verification, governed remote boundary, and
preflight report are implemented and locally verified. The erasure transaction
also removes pending, claimed, failed, and dead embedding work plus its attempt
history, so an in-flight stale worker cannot recreate vectors after erasure.

The spec is not complete. `aicortex serve` still lacks provider configuration,
a concrete local inference engine, first-activation and model-change wiring,
the managed background-worker lifecycle, and the re-embed and drop operator
verbs (B-2, B-4, B-5, B-6, B-9). The product preflight report can read the
active revision, deployment-wide queue counts and oldest-pending age, and
per-scope coverage, but the pinned chassis has no product preflight extension
hook through which to invoke it without violating spec 010 B-3 and B-6. Rahi
0.4.0 also does not expose tenant-level queue counts or a product collector
hook in the chassis `/metrics` registry (B-3). Expected quarantine dead letters
are reported separately from failed work and do not degrade readiness. An
inactive revision's queued work completes
as a terminal no-op, so a scope-local drop does not consult the chassis's
deployment-global queue counts.

The current application has no first-activation wiring, so capture does not
yet stage embedding work. The storage and durable-work contracts are
implemented and tested without regressing the existing capture path.

FR-002 and FR-007 have direct tests. FR-003 has direct dead-letter transition
and library report coverage, but its application preflight reporting path
remains open with the chassis hook. FR-001, FR-004, FR-005, and FR-006 also
remain open: capture does not stage the job yet, and there is no booted
local-provider socket probe, denied-host application preflight fixture, or
query predicate until the runtime wiring and spec 016 recall implementation
exist. These are recorded as open requirements, not inferred from lower-level
unit tests.

## Verification

```verify:cli
cargo test -p aicortex-embed --locked
```
