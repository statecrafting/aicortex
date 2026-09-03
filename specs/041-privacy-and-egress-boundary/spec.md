---
id: "041-privacy-and-egress-boundary"
title: "The privacy boundary: what leaves the container, proved by the manifest rather than promised"
status: approved
kind: "constraint"
domain: "ops"
created: "2026-09-03"
authors: ["Bartek Kus"]
implementation: pending
risk: critical
wave: 4
depends_on:
  - "040-retrieval-evaluation"
constrains:
  - { flavor: invariant-freeze, unit: "apps/aicortex/manifest.toml", note: "the egress ceiling is the whole privacy claim; additions are reviewable" }
  - { flavor: invariant-freeze, unit: "crates/aicortex-embed/src/remote.rs", note: "no content leaves except through a declared, governed host" }
summary: >
  The product's claim is that the user's memory stays theirs. This spec
  turns that from a sentence in a README into a checkable property. In the
  default configuration the manifest declares no egress host at all, so the
  process can make no outbound call and a test proves it. Every feature that
  would need one names it, justifies it, and is off by default. Telemetry
  does not exist. Backups go where the operator configured and nowhere else.
  The boundary is stated once here and inherited by every spec that could
  cross it.
---

# 041: The privacy boundary

## 1. Purpose

Everything in this system is designed around the claim that a person can
run their own memory and nothing leaves. That claim is only as good as its
weakest default, and memory systems break it in predictable places: an
embedding call, a metadata extraction call, a summarizer, a crash reporter,
an update check, an analytics ping.

The chassis provides the mechanism, a declared capability ceiling with a
governed egress facade and ledgered denials (`rahi://015`). This spec
states what this product does with it.

## 2. Territory

No files of its own. It freezes the manifest's egress list and the remote
provider's call path, and every spec that could open a socket satisfies its
requirements in that spec's own tests.

## 3. Behavior

- **B-1 (the default is zero).** In the default configuration
  `manifest.toml` declares an empty `egress` list. With it empty, the
  governed facade denies every outbound call, so the local embedding
  provider, the deterministic extractor, local curation, and the whole
  retrieval path work with no network reachable at all.
- **B-2 (additions are named and justified).** A feature that requires
  egress adds one host pattern with a comment naming the spec and the
  reason. The diff is reviewable and small, which is the entire point of a
  declared ceiling.
- **B-3 (opt-in, never default).** A remote embedding provider, a
  model-based extractor, a remote summarizer, and any source adapter that
  fetches from a third party are all disabled unless configured. Enabling
  one without its host in the ceiling fails at boot, not at first use.
- **B-4 (no telemetry).** This product sends no usage data, no error
  reports, and no update checks. There is no configuration that enables
  any of them, so there is nothing to audit and nothing to disable.
- **B-5 (backups).** Backup and archive go to the operator's configured
  object storage through the chassis (`rahi://014`, `rahi://030`). That
  host is in the ceiling when configured and absent when not.
- **B-6 (what may cross, when it is enabled).** Only the minimum: for
  remote embedding, the chunk text; for a model-based extractor, the memory
  text. Never a scope's contents in bulk, never provenance, never
  identifiers of other memories, and never the ledger. A batch is bounded
  and the count is metered.
- **B-7 (visible state).** `preflight` and `/api/v1/status` report the
  effective ceiling, which optional features are enabled, and the count of
  outbound calls since boot per host. A user can see what their instance is
  doing without reading configuration files.
- **B-8 (denials are evidence).** A denied egress attempt is a ledgered
  Decision (`rahi://015`), so an attempt to reach an undeclared host leaves
  a record rather than a log line.
- **B-9 (the container boundary).** The image runs the app and rauthy and
  nothing else; no sidecar agent, no shell-based cron reaching outward, and
  no fetch at runtime except the model weights fetch of 015 B-5, which is
  performed by `preflight`, verified against a pinned digest, and skipped
  entirely when the weights are present.

## 4. Functional requirements

- **FR-001.** With the default configuration, an integration test asserts
  the process opens no outbound socket across a full capture, embed,
  extract, curate, and recall cycle, using a network namespace with no
  route.
- **FR-002.** Enabling a remote provider without its host in the ceiling
  fails `preflight` with a named capability error.
- **FR-003.** A grep test asserts no HTTP client is constructed outside the
  governed facade call sites.
- **FR-004.** `preflight` output lists the ceiling and the enabled optional
  features, and the test compares it against the manifest.
- **FR-005.** A denied outbound attempt appears in the ledger.

## 5. Acceptance criteria

- **AC-1.** `cargo test --workspace --locked` passes with the
  no-network integration test enabled.
- **AC-2.** The default image, run with no network route, serves capture
  and recall end to end.

## 6. Out of scope

Encryption at rest beyond the chassis's backup encryption, and disk
security, which are deployment concerns. Anonymization of stored content,
which the product does not perform.

## 7. Resolved decisions

- **D-1 (2026-09-03, this spec).** No telemetry of any kind, rather than
  opt-in telemetry. Opt-in telemetry still requires shipping the code that
  could send it, and the code's absence is the only claim that can be
  verified by someone who does not trust us.

## Verification

```verify:cli
cargo test --workspace --locked
```
