---
id: "041-privacy-and-egress-boundary"
title: "The privacy boundary: what leaves the container, proved by the manifest rather than promised"
status: draft
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
references:
  - { unit: { kind: file, path: "docs/design/00-lineage.md" }, role: context }
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
  and the count is metered. For the evaluation judge of 040 B-12 (and the
  optional reader and judge of 040 B-9), the query and the framed text of
  the memories being judged, and nothing else; a judge never receives a
  scope's identifiers, provenance rows, or captured queries in bulk.
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
- **B-10 (evaluation judge egress; added 2026-10-25, D-2).** The judge
  model of 040 B-12, and the reader and judge of 040 B-9's optional
  answer-accuracy stage, are optional features under B-2 and B-3: off by
  default, each enabled only by naming its host in the ceiling with a
  comment naming 040, and failing at boot when enabled without it. They
  run only in the evaluation runner's temporary instance or against a
  scope whose owner enabled them for that scope. Every call goes through
  the governed facade, is metered under B-7, and a denied call is
  ledgered under B-8. The default configuration still declares no host,
  so B-1 is unchanged.
- **B-11 (captured queries stay in the instance; added 2026-10-25, D-2).**
  Query capture (040 B-11, `gbrain://eval-capture`) is local storage, not
  egress, and is not telemetry under B-4 because no code path transmits a
  captured row: there is no host to declare and no configuration that
  sends one. Capture is off by default per scope and enabled only by the
  scope's owner. Captured text is scrubbed by default; a query holding a
  detected credential is not captured. Rows carry a retention window
  (default 30 days) after which they are deleted, and they are erased with
  their scope (014) and on the owner's request, like any memory content.
  The only way out is an operator export to a local file, which writes a
  ledgered Decision naming the operator and the row count. `preflight` and
  `/api/v1/status` report, per scope, whether capture is enabled and how
  many rows are held.

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
- **FR-006.** With query capture enabled on a scope, FR-001's no-route
  test still opens no outbound socket across a cycle that captures
  queries.
- **FR-007.** Enabling the evaluation judge without its host in the
  ceiling fails `preflight` with a named capability error, and with the
  default configuration the judge path is unreachable.
- **FR-008.** A captured row older than the retention window is gone after
  the retention pass, and an export writes one Decision naming the
  operator and the count.

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

- **D-2 (2026-10-25, amendment, pending owner review).** Added B-10, B-11,
  and FR-006 to FR-008, and widened B-6, to carry the egress and
  retention terms of 040's evaluation additions (040 D-3). The judge is a
  remote model call, so it is an optional feature under B-2 and B-3 rather
  than a new kind of exception. Query capture is kept inside the instance:
  the observed system lets contributors ship captured queries to a
  separate evaluation repository, which this product does not carry,
  because B-4 and D-1 hold only if no code path can transmit usage data.
  The amendment adds behavior to an approved spec that no human has
  reviewed, so this spec returns to `status: draft` (constitution V).

## Verification

```verify:cli
cargo test --workspace --locked
```
