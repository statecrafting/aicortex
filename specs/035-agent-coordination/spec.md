---
id: "035-agent-coordination"
title: "Agent coordination: per-agent identity, work claims, and a shared working memory"
status: approved
kind: "feature"
domain: "clients"
created: "2026-09-03"
authors: ["Bartek Kus"]
implementation: pending
risk: medium
wave: 3
depends_on:
  - "034-curation-workers"
establishes:
  - "crates/aicortex-api/src/agents.rs"
  - "crates/aicortex-api/src/claims.rs"
  - "crates/aicortex-store/src/claims_repo.rs"
  - "crates/aicortex-api/tests/claims.rs"
extends:
  - { spec: "020-http-api-and-scopes", unit: "crates/aicortex-api/src/router.rs", nature: additive }
  - { spec: "012-store-schema-and-repositories", unit: "crates/aicortex-store/src/migrations.rs", nature: additive }
summary: >
  When several agents work in one scope they need to know who did what and
  to avoid doing the same thing twice. Per-agent identity already exists,
  because each client is its own OAuth client; this spec adds the two things
  that build on it. A work claim is a lease over a named unit of work with a
  fencing token, so two agents cannot both take it. A working-memory scope
  is a short-lived, high-churn area where agents post what they are doing,
  visible to their siblings and expired automatically, so coordination does
  not pollute durable memory.
---

# 035: Agent coordination

## 1. Purpose

The predecessor's ecosystem contained three independent agent-memory
integrations plus per-agent identity, work claims, and workflow status
schemas, all reinventing the same primitives (`openbrain://valued-capabilities`).
The sibling orchestrator has the same need today and solves it with files
in a working directory.

The primitives are small and the chassis already provides the hard part:
leases with fencing tokens over a Raft group.

## 2. Territory

Two API modules, one repository, and the `work_claim` and
`working_memory` tables.

## 3. Behavior

- **B-1 (agent identity).** An agent is an OAuth client (022 B-5) with a
  registered display name and the human subject that owns it. Memories it
  writes carry an `Agent` actor naming the client and, when supplied, the
  model. `GET /agents` lists the agents active in a scope with their last
  activity.
- **B-2 (work claim).** `POST /claims` takes a scope, a claim key, and a
  requested duration, and returns a claim with rahi's fencing token or 409
  with the current holder. `PATCH` renews, `DELETE` releases. A claim is a
  chassis lease, so the guarantee is the Raft group's, not this product's.
- **B-3 (fencing is mandatory).** A write that a claim guards carries the
  token and is rejected when the token is stale, which is the only correct
  way to survive a holder that paused and resumed after its lease expired.
- **B-4 (claims are visible).** `GET /claims` shows every active claim in a
  scope with holder, key, expiry, and the last renewal, so an operator can
  see a stuck agent.
- **B-5 (working memory).** A `working` scope kind whose memories carry a
  mandatory short time to live (default one hour, maximum one day),
  automatic expiry, no embedding, and no curation. It is for status,
  intent, and hand-off notes between agents in one piece of work.
- **B-6 (separation).** Working memory never enters durable retrieval:
  recall over a durable scope does not return it, and the two are separate
  scope kinds rather than a flag, so the separation is structural.
  Promoting a working note to a durable memory is an explicit capture.
- **B-7 (trust).** Everything here is an `Assertion` by an `Agent` actor.
  Nothing in this spec can produce `Instruction` grade.
- **B-8 (limits).** Claims per scope, working memories per scope, and
  renewal rates are all bounded, so a runaway agent cannot exhaust the
  store.

## 4. Functional requirements

- **FR-001.** Two agents requesting one claim key: one succeeds, one gets
  409 naming the holder and the expiry.
- **FR-002.** A write with a stale fencing token is rejected after the
  original holder's lease has been taken over.
- **FR-003.** A working memory expires automatically at its time to live
  and is absent from every durable recall before and after.
- **FR-004.** Working memories are never embedded, asserted by the absence
  of outbox rows for them.
- **FR-005.** An agent cannot create a memory with `Instruction` trust in
  any route in this spec.

## 5. Acceptance criteria

- **AC-1.** `cargo test -p aicortex-api --locked --test claims` passes.
- **AC-2.** Two concurrent agent clients coordinate over a claim key end
  to end against a running instance without both proceeding.

## 6. Out of scope

Task scheduling and orchestration, which belong to the orchestrator.
Delegation chains between agent principals, which are a sibling product's
concern and would wrap the subject this product already uses.

## 7. Resolved decisions

- **D-1 (2026-09-03, this spec).** Working memory is a scope kind rather
  than a flag on a memory. A flag is forgotten by one query and coordination
  chatter then leaks into recall forever; a separate scope kind cannot be
  forgotten because the query names the scope.

## Verification

```verify:cli
cargo test -p aicortex-api --locked --test claims
```
