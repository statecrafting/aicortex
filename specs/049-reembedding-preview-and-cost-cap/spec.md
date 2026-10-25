---
id: "049-reembedding-preview-and-cost-cap"
title: "Re-embedding: a dry run that reports counts and cost, and an approved total cap before any remote call"
status: draft
kind: "feature"
domain: "memory"
created: "2026-10-25"
authors: ["Bartek Kus"]
implementation: pending
risk: high
wave: 4
depends_on:
  - "015-embedding-pipeline"
  - "041-privacy-and-egress-boundary"
establishes:
  - "crates/aicortex-embed/src/preview.rs"
  - "crates/aicortex-embed/src/spend.rs"
  - "crates/aicortex-embed/tests/preview.rs"
extends:
  - { spec: "015-embedding-pipeline", unit: "crates/aicortex-embed/src/lib.rs", nature: additive }
  - { spec: "015-embedding-pipeline", unit: "crates/aicortex-embed/src/operator.rs", nature: additive }
  - { spec: "015-embedding-pipeline", unit: "crates/aicortex-embed/src/activation.rs", nature: additive }
  - { spec: "015-embedding-pipeline", unit: "crates/aicortex-embed/src/worker.rs", nature: additive }
  - { spec: "015-embedding-pipeline", unit: "crates/aicortex-embed/src/config.rs", nature: additive }
  - { spec: "015-embedding-pipeline", unit: "crates/aicortex-embed/src/migrations.rs", nature: additive }
amends:
  - "015-embedding-pipeline"
amends_sections:
  - "3-behavior"
references:
  - { unit: { kind: file, path: "docs/design/00-lineage.md" }, role: context }
  - { unit: { kind: file, path: "specs/041-privacy-and-egress-boundary/spec.md" }, role: constraint }
summary: >
  Spec 015 B-9 makes a model change a migration: a new revision is marked
  active and a re-embed pass runs over the scope. With the local provider
  that pass costs CPU time. With a remote provider it sends every chunk of
  a scope out and is billed per token, and nothing in 015 tells the
  operator how much before it starts or stops it at an amount they chose.
  This spec amends 015 B-9: every re-embed pass can be previewed with no
  provider call, reporting the work and its estimated cost, and a pass
  that would call a remote provider is refused until an operator approves a
  finite total cost cap, which admission enforces batch by batch.
---

# 049: Re-embedding preview and cost cap

## 1. Purpose

A re-embedding pass is the most expensive operation this product can start
with one call: it touches every chunk of a scope. gbrain's migration path
treats it accordingly (`gbrain://embedding-migration-consent`): a preview
that reports counts and estimated cost with no provider call, then an
operator-approved finite total cost cap, where consent to run is not a
substitute for the cap and raising the cap authorizes a larger total
rather than resetting what was already spent.

Spec 015 is built and complete, and today its build links no remote
transport (015 D-27), so no pass can spend money yet. That is exactly the
moment to fix the rule: the cap has to exist before the first remote
transport does, or the first remote re-embed will run without one. This
spec serves constitution XV, whose declared ceiling bounds where content
may go but not how much of it, and 041 B-6, which says a batch is bounded
and metered.

## 2. Territory

A `preview` module and a `spend` module in `aicortex-embed`, with tests.
Additive extensions of 015's operator routes (a dry-run form and a cap
parameter on the re-embed verb, a status field), its activation path (a
remote revision enqueues no billable work without a cap), its worker
(admission before each remote batch), its configuration (declared prices),
and its migrations (one table for authorizations and debits). 015's own
behavior for the local provider is unchanged.

## 3. Behavior

- **B-1 (amends 015 B-9).** Where 015 B-9 says changing the active model
  "enqueues a re-embed pass over the scope", this spec's B-2 to B-8 govern
  how that pass is previewed, authorized, and admitted. For a local
  provider the pass is enqueued as before; for a remote provider it is
  enqueued only under an authorization (B-4).
- **B-2 (preview).** `POST /operator/embedding/reembed` with `dry_run:
  true` reports, per scope and for the target revision: memories, chunks,
  chunks already embedded under the target revision, chunks to embed,
  estimated input tokens, and estimated cost. It makes no provider call,
  writes nothing, and enqueues nothing. The estimate uses the provider's
  declared token counting and its declared price per unit from
  configuration; when either is missing the preview says `pricing
  unknown` rather than guessing.
- **B-3 (local costs nothing).** A local provider's preview reports a cost
  of zero and needs no cap. Its pass runs as 015 B-9 describes.
- **B-4 (the cap is required for remote work).** A re-embed pass that
  would call a remote provider is refused before any call unless the
  request carries a finite, non-negative total cost cap approved by an
  authenticated operator. Unknown pricing refuses the pass. The
  authorization is recorded as a ledger Decision naming the operator
  subject, the scope, the target revision, the cap, and the preview it was
  approved against. Confirming the verb is not a cap, and no configuration
  default supplies one.
- **B-5 (admission per batch).** Before each remote batch the worker
  debits a conservative ceiling for that batch (its token count at the
  provider's maximum price, with the declared request overhead) against
  what remains of the authorization. A batch that would cross the cap is
  not dispatched; the pass pauses, reports `cap_reached` with the spent and
  remaining amounts, and leaves its outbox rows pending, so no memory is
  lost (constitution XI). After a batch completes, the debit is settled to
  the provider's reported usage where it reports one, and never below
  zero.
- **B-6 (the cap is a total).** An authorization covers one `(scope,
  target revision)`. Raising it records a new Decision for a larger total;
  it never resets the amount already spent. Spent amounts survive restarts
  and leader changes because they live in the store.
- **B-7 (visible).** `GET /operator/embedding/status` and `preflight`
  report, per scope with a remote pass, the authorized total, the spent
  amount, the remaining amount, and whether the pass is paused at its cap.
- **B-8 (ordinary capture is unchanged).** Embedding a newly captured
  memory under the active revision is not a re-embed pass and is not
  governed here; it remains under 015 and 041's metering.

## 4. Functional requirements

- **FR-001.** A dry run over a fixture scope reports the recorded counts
  and estimate, and a fake remote provider records zero calls.
- **FR-002.** A remote re-embed request without a cap, with a negative or
  non-finite cap, or with unknown pricing is refused, and the fake provider
  records zero calls.
- **FR-003.** With a cap that covers two of three batches, exactly two
  batches are dispatched, the pass reports `cap_reached`, and the third
  batch's outbox rows remain pending.
- **FR-004.** Raising the cap resumes the pass from where it stopped, and
  the reported spent amount is cumulative across both authorizations and
  across a restart.
- **FR-005.** A local provider's re-embed pass runs with no cap, exactly as
  015's tests describe.
- **FR-006.** Each authorization and each raise appears in the ledger once,
  naming the operator subject.

## 5. Acceptance criteria

- **AC-1.** `cargo test -p aicortex-embed --locked --test preview` passes.
- **AC-2.** `cargo test -p aicortex-embed --locked` passes, so 015's own
  acceptance still holds.

## 6. Out of scope

A remote transport itself, which 015 D-27 leaves unlinked and a later
spec adds. Cost limits on ordinary capture-time embedding, which belong
with 041's metering. Choosing or recommending a model. Pricing data
fetched from a provider at run time, which would be egress; prices are
operator configuration.

## 7. Resolved decisions

- **D-1 (2026-10-25, this spec).** A new spec with an `amends` edge on
  015 rather than an edit of 015 B-9. 015 is approved and complete; adding
  a requirement to it would make its built code non-conformant to its own
  text without any change to that code. This is the same instrument 048
  used to amend 001.
- **D-2 (2026-10-25, this spec).** Admission debits a conservative ceiling
  before each batch rather than the preview's estimate. The preview is an
  estimate and can be low; the cap is a promise and must not be.

## Verification

```verify:cli
cargo test -p aicortex-embed --locked --test preview
cargo test -p aicortex-embed --locked
```
