---
id: "020-http-api-and-scopes"
title: "The HTTP API: bearer-authorized, scope-gated, streaming where it earns it"
status: approved
kind: "kernel"
domain: "protocol"
created: "2026-09-03"
authors: ["Bartek Kus"]
implementation: pending
risk: critical
wave: 2
depends_on:
  - "019-untrusted-content-boundary"
establishes:
  - "crates/aicortex-api/Cargo.toml"
  - "crates/aicortex-api/src/lib.rs"
  - "crates/aicortex-api/src/router.rs"
  - "crates/aicortex-api/src/scopes.rs"
  - "crates/aicortex-api/src/capture.rs"
  - "crates/aicortex-api/src/search.rs"
  - "crates/aicortex-api/src/memories.rs"
  - "crates/aicortex-api/src/entities.rs"
  - "crates/aicortex-api/src/events.rs"
  - "crates/aicortex-api/src/dto.rs"
  - "crates/aicortex-api/src/error.rs"
  - "crates/aicortex-api/tests/api.rs"
  - "crates/aicortex-api/tests/authz.rs"
extends:
  - { spec: "010-chassis-adoption-and-workspace", unit: "apps/aicortex/src/cell.rs", nature: additive }
references:
  - { unit: { kind: file, path: "specs/019-untrusted-content-boundary/spec.md" }, role: constraint }
summary: >
  The product's own HTTP surface, mounted by the cell into rahi's edge. Every
  route is authorized by a rauthy-issued bearer token or a browser session,
  both resolved to the same principal, and gated by an OAuth scope that
  names what it permits. Scopes are coarse enough to be understandable and
  fine enough to be useful: read, write, delete, admin, and a separate scope
  for promotion, which no agent client is ever granted. Long operations
  report progress over server-sent events through the chassis's declared
  streaming routes. Every response carrying content obeys the untrusted
  content boundary.
---

# 020: The HTTP API

## 1. Purpose

The MCP surface is the way clients use this product, but MCP is not the only
consumer: importers, the orchestrator, a future dashboard, and the operator
all need HTTP. Building the HTTP layer first, with authorization decided
here once, means the MCP server (021) is a thin translation rather than a
second security boundary.

The predecessor's entire authorization model was one static key in a query
string that authorized everything for everyone forever
(`openbrain://static-shared-key`). This spec is the direct answer, and it is
possible only because the chassis grew a resource server for it
(`rahi://025`).

## 2. Territory

The `aicortex-api` crate. It depends on recall, store, gate, and graph, and
on rahi's edge and idp. It contains no SQL and no ranking, and it is the
only crate that defines a wire DTO.

## 3. Behavior

- **B-1 (mounting).** The router is returned by the cell's `routes` under
  `/api/v1`. It never constructs a server, a listener, or a middleware
  chain of its own; the chassis owns those.
- **B-2 (principal).** Both credential kinds resolve to rahi's `Principal`.
  A request's accessible scopes are those owned by the principal's `sub`
  plus those explicitly granted to it. A scope path parameter that the
  principal cannot reach answers 404, not 403, so scope existence does not
  leak.
- **B-3 (scopes as OAuth scopes).** `memory.read`, `memory.write`,
  `memory.delete`, `memory.admin`, and `memory.promote`. `RequireScope`
  from the chassis gates each route. `memory.promote` is the human
  boundary of 019 B-4: it is grantable to an interactive client and is
  refused to a client-credentials token by 025 B-4's service-callable
  rule.
- **B-4 (routes).** `POST /capture` (one or many candidates, returns the
  verdict per candidate and the merged or created id), `POST /search`
  (a `Query`, returns envelopes plus the trace summary), `GET /memories`
  (keyset paged list), `GET /memories/{id}`, `PATCH /memories/{id}`
  (correction and supersession), `DELETE /memories/{id}` (erasure),
  `GET /entities` and `GET /entities/{id}/neighbors`, `GET /stats`,
  `POST /promote/{id}`, and `GET /events` (see B-6).
- **B-5 (envelopes).** Every response that returns a body returns it inside
  the envelope of 019 with the framing statement in a sibling field, and
  never as a bare string. This is asserted by 019 FR-001 in this crate's
  test suite.
- **B-6 (streaming).** `GET /events` is a declared streaming route
  (`rahi://026`) carrying capture verdicts, embedding progress, curator
  activity, and review-queue changes for the caller's scopes. It is the
  progress surface for imports (031) and re-embedding (015 B-9). Events
  carry no memory bodies, only ids, counts, and states.
- **B-7 (errors).** One error type mapping to RFC 9457 problem documents
  with a stable `type` URI per reason code, so a client can branch on the
  gate's reason codes without parsing prose. A refusal from the gate is a
  422 with the reason code and no offending content.
- **B-8 (idempotency).** `POST /capture` accepts an `Idempotency-Key`
  header; a repeat within the retention window returns the original
  response. Combined with the fingerprint merge of 014 B-2, a retrying
  client cannot create duplicates.
- **B-9 (limits).** Batch capture is bounded (default 100 candidates),
  request bodies are bounded by the chassis, and the per-token rate limit
  group of `rahi://025` B-12 applies.
- **B-10 (versioning).** The path carries `v1`. A breaking change is `v2`
  mounted beside it; DTOs are additive within a version and unknown fields
  are rejected on input and ignored on output.

## 4. Functional requirements

- **FR-001.** A request with no credential answers 401 with the
  `WWW-Authenticate` challenge that names the resource metadata URL.
- **FR-002.** A token holding `memory.read` is refused on `POST /capture`
  with 403 and an `insufficient_scope` challenge naming `memory.write`.
- **FR-003.** A client-credentials token is refused on `POST /promote`
  regardless of its scopes.
- **FR-004.** A scope path the principal cannot reach answers 404 and
  emits no row-existence signal in timing or body.
- **FR-005.** The injection fixture of 019 FR-001 appears only inside an
  envelope in every content-returning route.
- **FR-006.** A repeated capture with the same idempotency key returns the
  first response and creates no second row.
- **FR-007.** `GET /events` delivers a keep-alive, closes on shutdown with
  the chassis's shutdown event, and carries no memory body.

## 5. Acceptance criteria

- **AC-1.** `cargo test -p aicortex-api --locked` passes.
- **AC-2.** With a rauthy binary on loopback, an end-to-end test registers
  a client, completes authorization code with PKCE, captures a memory, and
  searches for it.

## 6. Out of scope

MCP semantics (021), client install instructions (022), and the operator
surface, which the cell mounts on rahi's operator routes and which 043
documents.

## 7. Resolved decisions

- **D-1 (2026-09-03, this spec).** Five coarse scopes rather than
  per-tool scopes. A client asking for consent needs a screen a person can
  read; a scope per tool produces a consent screen nobody reads, which is
  worse than a coarse one they do.

## Verification

```verify:cli
cargo test -p aicortex-api --locked
```
