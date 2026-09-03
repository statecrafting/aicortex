---
id: "033-observatory-and-agent-ingestion"
title: "Machine intake: session decisions from the orchestrator and memories written by agents"
status: approved
kind: "feature"
domain: "ingest"
created: "2026-09-03"
authors: ["Bartek Kus"]
implementation: pending
risk: medium
wave: 3
depends_on:
  - "032-capture-sources-and-webhooks"
establishes:
  - "crates/aicortex-ingest/src/sources/agent_session.rs"
  - "crates/aicortex-ingest/src/sources/decision_feed.rs"
  - "crates/aicortex-ingest/src/schema/session-event.schema.json"
  - "crates/aicortex-ingest/tests/agent_session.rs"
extends:
  - { spec: "030-source-adapter-framework", unit: "crates/aicortex-ingest/src/registry.rs", nature: additive }
references:
  - { unit: { kind: file, path: "specs/019-untrusted-content-boundary/spec.md" }, role: constraint }
summary: >
  The most valuable memories in a software team are the decisions made
  inside driven sessions and then forgotten when the session ends. This spec
  accepts them: a generic session-event ingestion contract that any
  orchestrator or agent runtime can post to, with claude-observatory as its
  first producer, and a decision feed that turns sealed session decisions
  into memories. It is deliberately not Claude-specific, and it is
  deliberately a push into a documented schema rather than a reader of any
  particular tool's private files.
---

# 033: Machine intake

## 1. Purpose

The sibling orchestrator drives spec-bounded sessions and seals the
decisions they make. Those decisions are exactly the memories a team needs
next month: what was chosen, what was rejected, and why. Today they live in
one project's working directory and nothing recalls them across projects.

The design constraint is provider neutrality (002 §3). A reader that
watches one vendor's transcript directory is a Claude feature; a documented
event schema that any runtime posts to is a product.

## 2. Territory

Two adapter modules and one JSON schema inside `aicortex-ingest`.

## 3. Behavior

- **B-1 (the contract).** `session-event.schema.json` defines the posted
  event: producer id and version, session id, project, spec or task
  reference, event kind (`decision`, `correction`, `outcome`,
  `blocker`), actor (human or agent with model), timestamp, body, and
  optional links. It is versioned, additive, and validated on ingest;
  unknown producers are accepted, unknown event kinds are not.
- **B-2 (push, not scrape).** Producers post to
  `POST /api/v1/ingest/sessions` with their own OAuth client credentials
  and the `memory.write` scope. This product reads no other tool's private
  files.
- **B-3 (trust).** A `decision` produced by a human in a session is an
  `Assertion` by a `Human` actor. Everything produced by an agent is an
  `Assertion` by an `Agent` actor. Nothing from this path is ever
  `Evidence` about the world; it is evidence about what a session decided,
  which is recorded as such in the memory kind (`Decision`).
- **B-4 (mapping).** A session decision maps to a `Decision` memory whose
  provenance names the producer, the session, and the spec, and whose
  entities include the project and the spec. Corrections map to
  `Correction` memories that supersede what they correct (014 B-4) when the
  prior memory is identified.
- **B-5 (cross-project recall).** Session memories are scoped to a project
  scope by default and may be captured into a shared scope, so an
  orchestrator working in one repository can recall the decision another
  repository made. This is the feature that makes the family cohere.
- **B-6 (idempotency).** Events carry a producer-unique id; a replay is a
  no-op (030 B-4).
- **B-7 (the agent is not privileged).** An agent-produced memory is
  subject to the gate, cannot be promoted by its producer, and is returned
  under the same envelope as anything else. A driven session recalling its
  own prior decisions is reading untrusted data, and the framing says so.
- **B-8 (first producer).** claude-observatory is the first producer and
  its integration is documented in `clients/`. A second producer needs no
  change here.

## 4. Functional requirements

- **FR-001.** Events in the committed fixture set validate and map to the
  recorded memory kinds and actors.
- **FR-002.** An event with an unknown kind is rejected with a named
  error; one from an unknown producer is accepted and recorded.
- **FR-003.** A replayed event creates no second memory.
- **FR-004.** A memory produced by this path cannot be promoted by a
  client-credentials token, asserted end to end.
- **FR-005.** A cross-scope recall returns a decision captured from another
  project when the shared scope is reachable, and nothing when it is not.

## 5. Acceptance criteria

- **AC-1.** `cargo test -p aicortex-ingest --locked --test agent_session`
  passes.
- **AC-2.** claude-observatory posts the decisions of one real session and
  they are recallable, recorded in the spec's Status note.

## 6. Out of scope

Any change to the orchestrator, which is a separate repository. Reading
transcript files from a client's home directory, which this product refuses
as a design position.

## 7. Resolved decisions

- **D-1 (2026-09-03, this spec).** A push contract rather than a watcher.
  A watcher couples this product to one vendor's private file layout, which
  changes without notice and is unavailable for three of the four target
  clients.

## Verification

```verify:cli
cargo test -p aicortex-ingest --locked --test agent_session
```
