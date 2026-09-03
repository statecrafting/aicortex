---
id: "015-embedding-pipeline"
title: "Embeddings: a local default, a governed provider trait, an outbox worker that cannot lose a memory"
status: approved
kind: "kernel"
domain: "memory"
created: "2026-09-03"
authors: ["Bartek Kus"]
implementation: pending
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

## Verification

```verify:cli
cargo test -p aicortex-embed --locked
```
