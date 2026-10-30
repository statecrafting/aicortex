---
id: "020-http-api-and-scopes"
title: "The HTTP API: bearer-authorized, scope-gated, streaming where it earns it"
status: approved
kind: "kernel"
domain: "protocol"
created: "2026-09-03"
authors: ["Bartek Kus"]
implementation: in-progress
risk: critical
wave: 2
depends_on:
  - "015-embedding-pipeline"
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
  - "crates/aicortex-api/tests/common/mod.rs"
extends:
  - { spec: "010-chassis-adoption-and-workspace", unit: "apps/aicortex/src/cell.rs", nature: additive }
  - { spec: "010-chassis-adoption-and-workspace", unit: "apps/aicortex/Cargo.toml", nature: additive }
  - { spec: "010-chassis-adoption-and-workspace", unit: "Cargo.toml", nature: additive }
references:
  - { unit: { kind: file, path: "specs/019-untrusted-content-boundary/spec.md" }, role: constraint }
summary: >
  The product's own HTTP surface, mounted by the cell into rahi's edge. Every
  route is authorized by a rauthy-issued bearer token or a browser session,
  both resolved to the same principal, and gated by an OAuth scope that
  names what it permits. Scopes are coarse enough to be understandable and
  fine enough to be useful: read, write, delete, admin, a separate scope
  for promotion, which no agent client is ever granted, and a read and a
  write scope for coordination, which no memory scope implies. Long operations
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
  rule. Two coordination scopes complete the list (amended 2026-09-12,
  D-2): `coordination.read` for coordination views and `coordination.write`
  for coordination submissions, transitions, and work claims (035). No
  `memory.*` scope satisfies either, and neither satisfies a `memory.*`
  scope. A coordination scope is necessary, never sufficient: the owner,
  recipient, and resolver grants of the coordination scope still apply, and
  resolving a request whose acceptance names a human decision is refused to
  a client-credentials token under the same rule as `memory.promote`.
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
- **FR-008.** `scopes.rs` enumerates exactly the seven scopes of B-3, and a
  table test asserts that no `memory.*` scope satisfies a `coordination.*`
  requirement and no `coordination.*` scope satisfies a `memory.*` one.
- **FR-009 (added 2026-10-10, 015 D-33).** With the local embedding
  provider selected and its artifacts in place, a test boots `aicortex
  serve`, captures a memory through `POST /capture`, waits for its
  embedding, and asserts that the process opened no outbound socket, and
  that the shipped manifest declares no egress host. This is 015 FR-004's
  whole-process form, which moved here because this spec creates the first
  capture route.

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

- **D-1 (2026-09-03, this spec; amended 2026-09-12, D-2).** Coarse scopes
  rather than per-tool scopes: five for memory, and since D-2 two for
  coordination. A client asking for consent needs a screen a person can
  read; a scope per tool produces a consent screen nobody reads, which is
  worse than a coarse one they do.
- **D-2 (2026-09-12, amendment, revision-4 AI-04).** Draft 045 needs
  coordination state (requests, transitions, resolutions, claims) that a
  memory writer must not be able to change merely by holding
  `memory.write`. The maintainer chose two separate coordination scopes
  over reusing the memory scopes, with explicit owner, recipient, and
  resolver grants inside the coordination scope, and interactive
  principals for any resolution that names a human decision. Two scopes
  keep the consent screen readable, which is D-1's reason for coarse
  scopes in the first place. The grant table itself and the routes that
  use these scopes are 045's; 035's claim routes use them as amended there.
  The maintainer adopted this on 2026-09-12; the agent authored the text
  and this entry records that authority rather than assuming it.

- **D-3 (2026-10-10, amendment, approved by the owner 2026-10-10).** Owner
  direction 2026-10-10: 020 and 021 follow 015 directly, ahead of 016 to
  018, so local agents and machine intake have a surface before ranked
  recall exists.
  - **Order.** 020 depends on 015 and on 019 (which now depends on 015,
    019 D-2). After 015 completes, the build order is 019, 020, 021, then
    016, 017, 018. Because the lowest-number rule of `AGENTS.md` would pick
    016 first, this decision is the recorded override: while 019, 020, or
    021 is ready and pending, it is picked before 016 to 018.
  - **Staged routes.** This spec's build ships B-1 to B-3 and B-5 to B-10
    and the B-4 routes that need no ranking or graph: `POST /capture`,
    `GET /memories`, `GET /memories/{id}`, `PATCH /memories/{id}`,
    `DELETE /memories/{id}`, `GET /stats`, `POST /promote/{id}`, and
    `GET /events`. `POST /search` is mounted by 018's build session and
    `GET /entities` and `GET /entities/{id}/neighbors` by 017's, each
    through an `extends` edge on this crate's router and DTOs, and each
    carrying FR-005 for its route. AC-2's final step reads the captured
    memory back with `GET /memories/{id}` until 018 lands, and 018's build
    restores "searches for it".
  - **What does not change.** Authorization, scopes, the envelope rule, and
    every FR. FR-005 binds on every content-returning route that exists.
    FR-009, added after this entry by 015 D-33, adds a test and changes
    no route.

- **D-4 (2026-10-29, this spec, build).** The spec does not say how a
  request names a scope. A scope is named by `kind` plus its `project` or
  `share` key, in the body of a write and in the query string of a read,
  and its `owner` defaults to the caller. No grant table exists yet (012
  B-9 makes it a future spec with its own predicate), so the scopes "explicitly
  granted" by B-2 are none today and a scope is reachable exactly when the
  caller owns it. An unreachable scope is a 404 decided before any store
  read, with one fixed document for every case, which is what keeps FR-004's
  body and timing free of a row-existence signal. Rejected: a scope path
  segment (`/scopes/{scope}/memories`), which B-4's route list does not have.
- **D-5 (2026-10-29, this spec, build).** The cell declares `/api/v1` a
  service-callable bearer route and `/api/v1/promote` a person-only one,
  so importers and the orchestrator holding a client credentials token reach
  capture and the listing, and the chassis's longest-prefix rule refuses
  that token on promotion before the handler runs (FR-003, 025 B-4). The
  handler refuses it again. Rejected: a person-only `/api/v1`, which would
  shut the machine consumers of section 1 out of the whole surface.
- **D-6 (2026-10-29, this spec, build).** `memory.admin` and the two
  coordination scopes are enumerated (FR-008) and gate no route of this
  build: no admin route is in B-4, and the coordination routes are 035's and
  045's. Only a scope some mounted route requires is published in
  `scopes_supported`, because a gate publishes its scope when it is built.
- **D-7 (2026-10-29, this spec, build).** The idempotency record of B-8 lives
  in the chassis's replicated cache for 24 hours, keyed by a digest of the
  subject, the client, and the key, and bound to a digest of the request
  body: the same key with a different body is a 422
  `idempotency_key_reused`. A server failure is not remembered, so a retry
  tries again. The cache is derived state; losing a record costs only the
  replay, because the retried capture merges by fingerprint (014 B-2) and
  creates no second row. Rejected: a new table, which would put SQL and a
  migration in a crate whose territory has none.
- **D-8 (2026-10-29, this spec, build).** `GET /events` is fed by a node-local
  bus. The store's notify is a single-reader hint stream (`rahi://012` B-3)
  and cannot fan out to many subscribers. This build publishes capture
  verdicts, and the memory routes add state changes (superseded, erased),
  filtered to the caller's own scopes, as ids, codes, and states. A lagging subscriber gets
  a `lagged` event with no count, because the count would describe other
  subjects' traffic.
- **D-9 (2026-10-29, this spec, build).** A capture of one candidate that
  the gate refuses is the 422 problem of B-7. A batch answers 200 with one
  outcome per candidate, because each candidate is its own transaction and
  its neighbours may have been stored; a refused outcome carries its reason
  code and the problem `type` of B-7. No answer echoes refused content.
- **D-10 (2026-10-29, this spec, build).** Two chassis behaviours at rahi
  0.7.1 bound what B-2 and FR-001 can mean here, and this build consumes
  rather than works around them. First, `RequireScope` reads only a bearer
  token's scopes, so a browser session principal is refused by every scope
  gate: the session half of B-2 needs a chassis change before a browser
  surface exists. Second, an unsafe request carrying no credential at all
  meets the edge's CSRF check before any 401, so FR-001's challenge is
  answered on a safe request; an unsafe one is refused 403 and stores
  nothing. Both are recorded for rahi rather than patched here.

## Status

- **2026-10-29.** `implementation: in-progress`. This spec lands in two
  changes, each under the size the delivery policy sets. The first, this
  one, founds the crate and mounts it: B-1 to B-3, `POST /capture` with B-7
  to B-10, `GET /events` (B-6), and FR-001, FR-002, FR-004, FR-006 to
  FR-008, all under AC-1. Remaining before `complete`:
  - **The rest of the D-3 routes**, in the second change: `GET /memories`,
    `GET /memories/{id}`, `PATCH /memories/{id}`, `DELETE /memories/{id}`,
    `GET /stats`, and `POST /promote/{id}`, with FR-003 and FR-005.
  - **AC-2** needs a rauthy binary on loopback. None exists on the build
    host (macOS; rahi pins a Linux image) or in CI, so the end-to-end
    authorization code with PKCE run has not been performed.
  - **FR-009** boots `aicortex serve` and captures over HTTP, which needs
    composed identity: the same rauthy binary, or a discovery stub that
    `serve` accepts.
  - **B-6** carries capture verdicts; embedding progress, curator
    activity, and review-queue changes are not published yet (D-8).

## Verification

```verify:cli
cargo test -p aicortex-api --locked
```
