---
id: "043-packaging-and-operations"
title: "Packaging: one image on the chassis, the operator surface, the runbook"
status: approved
kind: "tooling"
domain: "ops"
created: "2026-09-03"
authors: ["Bartek Kus"]
implementation: pending
risk: high
wave: 4
depends_on:
  - "042-portability-and-migration"
establishes:
  - "docker/Dockerfile"
  - "docker/compose.yml"
  - "apps/aicortex/src/operator.rs"
  - "docs/operations.md"
  - "docs/configuration.md"
  - ".github/workflows/image.yml"
extends:
  - { spec: "010-chassis-adoption-and-workspace", unit: "apps/aicortex/src/cell.rs", nature: additive }
summary: >
  The chassis already owns the image shape, the volume, the supervision, the
  four verbs, the probes, and the metrics. This spec adds only what is this
  product's: the build that produces the binary and fetches or embeds the
  local model, the operator routes the cell mounts, the configuration
  reference, and the runbook for the handful of things that can go wrong.
  It is deliberately thin, because a packaging spec that grows is a sign the
  chassis is being reimplemented.
---

# 043: Packaging and operations

## 1. Purpose

Everything structural about deployment is inherited (`rahi://031`,
`rahi://030`). What remains is this product's own operational surface: what
an operator can see and do, what the knobs mean, and what to do when the
dead-letter count is not zero.

## 2. Territory

The image build, the compose file for local use, the operator module, and
two documents.

## 3. Behavior

- **B-1 (the image).** A multi-stage build producing the chassis's
  single-container shape: the `aicortex` binary and the rauthy release
  binary, one volume, one origin, die-together supervision, multi-arch.
  This spec adds no second process.
- **B-2 (model weights).** The image ships without weights by default and
  `preflight` fetches them once to the volume against a pinned digest (015
  B-5). A build argument produces a fat variant with weights embedded for
  air-gapped installs, and both variants report which they are.
- **B-3 (operator routes).** Mounted on the chassis's operator surface,
  gated by its operator role: dead-letter listing and requeue, re-embed
  start and status, curator run and undo, source run control, scope
  listing with counts, export trigger, and the effective privacy state of
  041 B-7. Every one of them is an existing capability exposed to an
  operator, not new behavior.
- **B-4 (configuration reference).** `docs/configuration.md` documents
  every setting with its default, its range, whether it can be changed at
  run time, and which spec owns it. A setting absent from this document is
  a defect, asserted by a test that compares the document against the
  config type.
- **B-5 (metrics).** Product metrics on the chassis's endpoint: memories
  by scope and status, capture rate, gate verdicts by reason, embedding
  queue depth and dead letters, recall latency by channel, truncation
  rate, curator run outcomes, review queue depth, and outbound calls by
  host. Names are stable and documented.
- **B-6 (readiness).** The product contributes to the chassis's readiness
  probe: schema current, active embedding model resolvable, dead-letter
  count under threshold. A degraded state is reported rather than hidden.
- **B-7 (the runbook).** `docs/operations.md` covers first boot, adding a
  user, connecting a client, backup and restore through the chassis verbs,
  changing the embedding model, draining dead letters, undoing a curator
  run, and reading the privacy state. Each entry is a command and an
  expected output, not prose.
- **B-8 (upgrade).** An upgrade is a new image and `migrate`. The chassis
  version is a pinned dependency, so a chassis upgrade is a version bump
  with its own release note.

## 4. Functional requirements

- **FR-001.** The image builds for both architectures in CI and the
  resulting container passes `preflight` on an empty volume.
- **FR-002.** `docs/configuration.md` covers every field of the config
  type, asserted by test.
- **FR-003.** Every operator route requires the operator role and is
  absent from the public router.
- **FR-004.** `/metrics` exposes every documented product metric after the
  integration suite runs.
- **FR-005.** The fat image variant runs with no network route and no
  weights fetch.

## 5. Acceptance criteria

- **AC-1.** `cargo test -p aicortex --locked` passes.
- **AC-2.** `docker compose up` yields a working instance: sign in,
  connect a client, capture, recall.

## 6. Out of scope

Cluster topology and the reference deployment (044). Everything the
chassis owns: the volume layout, key handling, supervision, backup
mechanics, and the verbs themselves.

## 7. Resolved decisions

- **D-1 (2026-09-03, this spec).** Two image variants rather than one fat
  image. The default image stays small enough to pull quickly, and the
  air-gapped case is a documented build argument rather than a fork.

## Verification

```verify:cli
cargo test -p aicortex --locked
```
