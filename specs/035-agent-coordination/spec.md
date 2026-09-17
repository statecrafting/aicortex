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
  that build on it. A work claim is a renewable, application-owned hold on a
  named unit of work with its own fencing token, so two agents cannot both
  take it. A working-memory scope
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

The primitives are small and the chassis already provides the hard part: a
replicated store with transactions, and a short lease that serializes
contenders. The renewable claim on top of them is a row this product owns
(amended 2026-09-12, D-2).

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
  requested duration, and returns a claim with its claim token or 409
  naming the current holder and expiry. `PATCH` renews, `DELETE` releases.
  A claim is an application row in `work_claim` (scope, key, holder,
  expiry, claim token, last renewal), not a chassis lease (amended
  2026-09-12, D-2). Every claim change runs under rahi's short lease on the
  claim key, which serializes contenders, and commits the row in one
  transaction. The claim token belongs to the key: it increases on every
  takeover, meaning any acquisition after expiry or release, including one
  by the previous holder, and never on renewal. Rahi's lease fence token
  is never returned as, stored as, or compared with a claim token, because
  every lease acquisition, including a refused competitor's, mints a new
  one. The guarantee is the Raft group's transaction over this row.
- **B-3 (fencing is mandatory).** A write that a claim guards carries the
  claim token, and the token is checked against the `work_claim` row inside
  the same transaction as the write. A stale token or an expired claim
  rejects the write, which is the only correct way to survive a holder that
  paused and resumed after its claim expired.
- **B-4 (claims are visible).** `GET /claims` shows every active claim in a
  scope with holder, key, expiry, claim token, and the last renewal, so an
  operator can see a stuck agent (amended 2026-09-12, D-2).
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
- **B-9 (claims are coordination state).** The claim routes require
  `coordination.write` to take, renew, or release and `coordination.read`
  to list (020 B-3); `memory.write` alone takes no claim. A claim is live
  state: portable export (042) never includes a claim, a claim token, or a
  grant, and import never restores one (amended 2026-09-12, D-3).

## 4. Functional requirements

- **FR-001.** Two agents requesting one claim key: one succeeds, one gets
  409 naming the holder and the expiry.
- **FR-002.** A write with a stale claim token is rejected after the
  original holder's claim has expired and been taken over.
- **FR-006.** Renewing a claim leaves its claim token unchanged; a takeover
  after expiry or release increases it by exactly one.
- **FR-007.** A competitor's refused `POST /claims` against a held key
  leaves the holder's claim token unchanged, and the holder's next guarded
  write succeeds.
- **FR-008.** A token holding `memory.write` without `coordination.write`
  is refused on `POST /claims` with 403 naming `coordination.write`.
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
- **D-2 (2026-09-12, amendment, revision-4 AI-02).** B-2 said a claim is a
  chassis lease with renewal. Rahi's lease does not support that: its
  time to live is ten seconds and documented as never configurable, the
  expiry is set once at acquisition and never refreshed, no renew method
  exists, and every acquisition mints a fresh fence token, so a refused
  competitor would supersede the rightful holder's token. The maintainer
  chose application claim rows with their own per-key token, increased on
  takeover and not on renewal, checked inside the guarded transaction, with
  rahi's lease used only to serialize claim changes. Rejected: asking rahi
  for a renewable lease API, which would take a chassis spec and release
  to meet a need an application row already meets. B-2, B-3, B-4, the
  summary, and section 1 change with it; FR-006 and FR-007 pin the two
  properties the chassis token could not give.
- **D-3 (2026-09-12, amendment, revision-4 AI-04).** Claims protect
  coordination work, so they sit behind 020's coordination scopes rather
  than `memory.write`, and as live state they are never exported or
  imported. Working memory stays under the memory scopes: it is memory of a
  `working` kind (B-5), not coordination state. The maintainer adopted D-2
  and D-3 on 2026-09-12; the agent authored the text and these entries
  record that authority rather than assuming it.

## Verification

```verify:cli
cargo test -p aicortex-api --locked --test claims
```
