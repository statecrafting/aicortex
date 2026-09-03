---
id: "044-reference-deployment"
title: "The reference deployment: three nodes on Kubernetes, object storage behind it, evaluation green"
status: approved
kind: "tooling"
domain: "ops"
created: "2026-09-03"
authors: ["Bartek Kus"]
implementation: pending
risk: high
wave: 4
depends_on:
  - "043-packaging-and-operations"
establishes:
  - "deploy/k8s/kustomization.yaml"
  - "deploy/k8s/statefulset.yaml"
  - "deploy/k8s/service.yaml"
  - "deploy/k8s/ingress.yaml"
  - "deploy/k8s/backup-cronjob.yaml"
  - "deploy/k8s/servicemonitor.yaml"
  - "deploy/k8s/secret.example.yaml"
  - "deploy/README.md"
  - "scripts/k8s-validate.sh"
  - "crates/aicortex-harness/Cargo.toml"
  - "crates/aicortex-harness/src/lib.rs"
  - "crates/aicortex-harness/tests/e2e.rs"
summary: >
  The wave 4 exit condition and the product's own proof. A three-node
  StatefulSet on Kubernetes with object storage for backups and archived
  ledger segments, one origin behind an ingress, metrics scraped, backups on
  a schedule, and an end-to-end harness that drives the whole product
  through a real client path: sign in, connect, capture, import, curate,
  recall, promote, export, and erase. When this is green the specification
  has been demonstrated rather than argued.
---

# 044: The reference deployment

## 1. Purpose

Every prior spec is verifiable in isolation. This one asserts they compose:
that the cell really is a cell, that three replicas really do agree, that a
backup really does restore, and that a user really can go from an empty
instance to a useful memory in one sitting.

## 2. Territory

The Kubernetes manifests, the validation script, and the end-to-end
harness crate. The topology itself is the chassis's (`rahi://032`); this
spec configures it and proves the product on top.

## 3. Behavior

- **B-1 (topology).** A three-replica StatefulSet, one volume per replica,
  the app and rauthy in one pod as the chassis prescribes, one Service and
  one Ingress presenting a single origin with TLS. Resource requests and
  limits are set and justified in `deploy/README.md`, sized for the
  embedding model's memory footprint.
- **B-2 (object storage).** Backups and archived ledger segments go to
  S3-compatible object storage configured by secret, with the endpoint in
  the manifest ceiling (041 B-5). The backup CronJob invokes the chassis's
  `backup` verb; restore is documented and rehearsed.
- **B-3 (secrets).** `secret.example.yaml` carries the shape and no
  values: object storage credentials, the rauthy bootstrap secret, and the
  optional remote provider key. Every value is required to be supplied;
  none has a default.
- **B-4 (observability).** A ServiceMonitor scrapes the chassis endpoint;
  `deploy/README.md` lists the alerts worth setting: dead letters above
  threshold, embedding queue depth growing, recall p99 above budget,
  ledger verification failure, backup age, and any outbound call to a host
  the operator did not expect.
- **B-5 (validation).** `scripts/k8s-validate.sh` renders the kustomization
  and validates it against the API schemas without a cluster, so CI can
  gate the manifests.
- **B-6 (the harness).** `aicortex-harness` boots the real binary through
  the chassis's harness and drives one full path: a user signs in through
  rauthy, registers a client dynamically, captures memories through MCP,
  imports a small export, runs a curator pass, recalls with a trace,
  promotes one memory as a human, exports the scope, erases one memory, and
  verifies the ledger. Every assertion is on observable output.
- **B-7 (the exit condition).** Wave 4 is complete when the harness passes
  against a three-node deployment and `make eval` is green on the same
  build. The result is recorded in this spec's Status note with the date
  and the image digest.
- **B-8 (single-node too).** The same manifests with one replica are the
  supported small deployment, and the harness runs against both, because
  most users will run one node and their path must be tested.

## 4. Functional requirements

- **FR-001.** `scripts/k8s-validate.sh` exits 0 and CI runs it.
- **FR-002.** The harness passes against a single-node deployment.
- **FR-003.** The harness passes against a three-node deployment, and a
  memory captured against one replica is recalled from another.
- **FR-004.** A backup taken from the cluster restores into an empty
  deployment, and the restored instance serves the same recall results.
- **FR-005.** Killing one replica leaves the service available and the
  ledger verifiable afterwards.
- **FR-006.** `make eval` is green against the deployed image.

## 5. Acceptance criteria

- **AC-1.** `cargo test -p aicortex-harness --locked` passes against a
  local single-node deployment.
- **AC-2.** The three-node run of B-7 is recorded with date and image
  digest.

## 6. Out of scope

Managed hosting, multi-tenant operation of many customers' cells, and
autoscaling, all of which are the sibling delivery plane's concerns rather
than this product's.

## 7. Resolved decisions

- **D-1 (2026-09-03, this spec).** The single-node path is tested as a
  first-class case rather than treated as a degraded cluster. It is what
  almost every user will run, and a product whose small deployment is
  untested will fail for most of its users.

## Verification

```verify:cli
scripts/k8s-validate.sh
cargo test -p aicortex-harness --locked
```
