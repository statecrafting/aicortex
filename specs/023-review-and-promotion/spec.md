---
id: "023-review-and-promotion"
title: "Review: the queue where quarantine, contradiction, and promotion wait for a human"
status: approved
kind: "feature"
domain: "memory"
created: "2026-09-03"
authors: ["Bartek Kus"]
implementation: pending
risk: high
wave: 2
depends_on:
  - "022-client-adapters"
establishes:
  - "crates/aicortex-curate/Cargo.toml"
  - "crates/aicortex-curate/src/lib.rs"
  - "crates/aicortex-curate/src/review.rs"
  - "crates/aicortex-curate/src/promotion.rs"
  - "crates/aicortex-curate/src/migrations.rs"
  - "crates/aicortex-curate/tests/review.rs"
extends:
  - { spec: "012-store-schema-and-repositories", unit: "crates/aicortex-store/src/migrations.rs", nature: additive }
  - { spec: "020-http-api-and-scopes", unit: "crates/aicortex-api/src/router.rs", nature: additive }
references:
  - { unit: { kind: file, path: "specs/019-untrusted-content-boundary/spec.md" }, role: constraint }
summary: >
  Three situations need a person: content whose origin could not be
  established and was quarantined, two edges that contradict each other, and
  a memory that someone wants a client to act on. This spec is the one queue
  where all three wait, the decisions that empty it, and the ledger records
  those decisions produce. Promotion is the important one: it is the only
  path to instruction grade, it requires the promote scope and an
  interactive principal, and it writes a rahi Decision whose id becomes part
  of the memory's trust.
---

# 023: Review and promotion

## 1. Purpose

Constitution X says a client may act only on memories a human promoted.
That is a policy until there is a place the human does it. This spec is
that place, and it deliberately batches the three human decisions the
system generates rather than inventing a surface per decision.

## 2. Territory

The `aicortex-curate` crate is founded here with the review queue and
promotion. Its curation workers arrive in 034. The queue's routes are added
to the API router.

## 3. Behavior

- **B-1 (the queue).** `review_item(id, scope, kind: ReviewKind, subject,
  created, state, resolved_by, decision_id)` where `ReviewKind` is
  `Quarantine`, `Contradiction`, `PromotionRequest`, or `GateOverride`.
  Items are created by the gate (013 B-7), the graph (017 B-10), the API,
  and the operator override path.
- **B-2 (routes).** `GET /review` (paged, filtered by kind and state),
  `POST /review/{id}/resolve` with a typed resolution per kind. The queue
  count is exposed as an MCP resource (021 B-8) so an assistant can tell
  the user there is something waiting, without being able to resolve it.
- **B-3 (quarantine resolution).** `Admit` moves the memory to `Active`
  with `trust: Assertion` and records who admitted it; `Reject` erases it
  through 014 B-7. Both write a Decision.
- **B-4 (contradiction resolution).** The resolver picks the surviving
  edge, supersedes the other, or marks both as historically valid with
  disjoint validity ranges. A Decision records the choice and the reason
  text.
- **B-5 (promotion).** `POST /promote/{id}` requires the `memory.promote`
  scope and an interactive principal (`rahi://025` B-4 refuses a
  client-credentials token). It appends a rahi Decision, receives the
  `DecisionId`, constructs the `Promotion` token, and writes the memory's
  trust class as `Instruction` with the decision id stored beside it. The
  `Promotion` type is the only bridge, and it cannot be produced any other
  way (011 B-5).
- **B-6 (demotion).** `POST /demote/{id}` returns a memory to `Assertion`
  and writes a Decision. Promotion is reversible; the ledger keeps both
  records.
- **B-7 (expiry of promotion).** A promotion may carry a review interval.
  When it lapses the memory returns to `Assertion` automatically and a
  review item is created, because a standing instruction nobody has
  reconfirmed in a year is not a standing instruction.
- **B-8 (no agent path).** Every route here requires an interactive
  principal. There is no MCP tool, no service-callable route, and no
  configuration that grants promotion to an automated client.
- **B-9 (envelopes).** Review items display memory content, so responses
  obey 019.

## 4. Functional requirements

- **FR-001.** A quarantined memory becomes retrievable only after an
  `Admit` resolution, and its trust class is `Assertion`, never higher.
- **FR-002.** A promotion by an interactive principal writes exactly one
  Decision, and the memory's trust class is `Instruction` with that
  decision id.
- **FR-003.** A promotion attempt by a client-credentials token is refused
  with 403 and writes no Decision that suggests success.
- **FR-004.** A lapsed promotion returns the memory to `Assertion` and
  creates a review item, driven by an injected clock.
- **FR-005.** Resolving a contradiction leaves exactly one active edge for
  the subject and predicate, or two with disjoint validity.

## 5. Acceptance criteria

- **AC-1.** `cargo test -p aicortex-curate --locked --test review` passes.
- **AC-2.** `aicortex ledger verify` succeeds after a promotion, a
  demotion, and a quarantine rejection.

## 6. Out of scope

Automated curation (034). A review user interface, which is a later
concern; the routes and the queue count are the surface for now.

## 7. Resolved decisions

- **D-1 (2026-09-03, this spec).** Promotion expires by default with a
  configurable interval, rather than lasting forever. A memory system's
  worst failure mode is a stale instruction, and the cost of reconfirmation
  is one queue item.

## Verification

```verify:cli
cargo test -p aicortex-curate --locked --test review
```
