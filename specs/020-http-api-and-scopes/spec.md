---
id: "020-http-api-and-scopes"
title: "The HTTP API: bearer-authorized, scope-gated, streaming where it earns it"
status: approved
kind: "kernel"
domain: "protocol"
created: "2026-09-03"
authors: ["Bartek Kus"]
implementation: complete
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
  - "crates/aicortex-api/tests/common/live.rs"
  - "crates/aicortex-api/tests/live.rs"
  - "apps/aicortex/tests/serve_capture.rs"
extends:
  - { spec: "010-chassis-adoption-and-workspace", unit: "apps/aicortex/src/cell.rs", nature: additive }
  - { spec: "010-chassis-adoption-and-workspace", unit: "apps/aicortex/Cargo.toml", nature: additive }
  - { spec: "010-chassis-adoption-and-workspace", unit: "Cargo.toml", nature: additive }
  - { spec: "012-store-schema-and-repositories", unit: "crates/aicortex-store/src/memory_repo.rs", nature: additive }
  - { spec: "012-store-schema-and-repositories", unit: "crates/aicortex-store/src/scope_repo.rs", nature: additive }
  - { spec: "015-embedding-pipeline", unit: "crates/aicortex-embed/src/worker.rs", nature: additive }
  - { spec: "015-embedding-pipeline", unit: "crates/aicortex-embed/src/lib.rs", nature: additive }
  - { spec: "015-embedding-pipeline", unit: "apps/aicortex/src/embedding_service.rs", nature: additive }
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
  verdicts and memory state changes (superseded, erased), filtered to the caller's own scopes, as ids, codes, and states. A lagging subscriber gets
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

- **D-11 (2026-10-29, this spec, build).** The listing cursor (012 B-7) is
  signed with a random key that the first node writes into the chassis's
  replicated cache, so every node verifies every other node's cursors. If
  the cache is cleared, outstanding cursors stop verifying and the client
  starts its listing again; no decision depends on a cursor. Rejected: a
  per-process key, which breaks paging behind a load balancer.
- **D-12 (2026-10-29, this spec, build).** `POST /promote/{id}` is mounted
  with its authorization (`memory.promote`, a person's token, a reachable
  scope) and answers 501 `promotion_unavailable` without writing anything.
  The act (appending the Decision, constructing the `Promotion`, writing the
  trust class) is 023 B-5's and needs a store write this crate's territory
  does not contain; 023's build replaces the 501.
- **D-13 (2026-10-29, this spec, build).** The `origin_erased` marker the
  envelope of 019 B-1 carries is a column beside the stored record, so the
  memory routes read it through a new `MemoryRepo::origin_erased` beside the
  memories they return (an additive `extends` edge on 012's repository): the
  SQL stays in the store crate, and a correction whose corrected memory was
  erased is labelled rather than presented as if its source still stood.

- **D-14 (2026-11-24, approved by the owner 2026-11-24).** AC-2 and FR-009
  run against a real rauthy on loopback, provided by a container. The image
  is the one rahi ships its cell with, pinned by digest in
  `scripts/live-rauthy.sh` (copied from rahi's `docker/Dockerfile`, not
  modified there); `scripts/live-rauthy.sh up` starts it with dynamic client
  registration, a bootstrap administrator, and an API key, and prints the
  `AICORTEX_TEST_RAUTHY_*` variables. The driver (dynamic registration,
  authorization code with PKCE, proof of work) is
  `crates/aicortex-api/tests/common/live.rs`, written to be included by
  another crate's tests with `#[path]` so 021 AC-2 reuses it. The
  registration token, administrator, and API key belong to the throwaway
  fixture; the product mints no credential and accepts no static key, and the
  token a test uses is the one rauthy signs after a PKCE login. A test skips
  without `AICORTEX_TEST_RAUTHY_URL`, and `AICORTEX_REQUIRE_RAUTHY=1` (set by
  `.github/workflows/live.yml`) makes a skip a failure. The workflow is
  advisory, as rahi's is. The fixture client's `default_aud` is the cell's
  public origin, which is how the pinned rauthy adds an audience (rahi 037
  D-7), and its access token lifetime is 300 seconds, under the cell's 600
  second ceiling.
- **D-15 (2026-11-24, approved by the owner 2026-11-24).** B-6 publishes the
  event kinds whose producers exist: capture verdicts, memory state changes,
  and embedding progress (`embedding` events with the memory id and a state
  of `embedded`, `retrying`, or `dead_lettered`; no body). Curator activity
  and review-queue changes are published by their producing specs (034 and
  023) onto the same bus through `extends` edges on this crate's events unit
  when those specs are built. This is a scoping of what is built now, not a
  weakening of any requirement. The worker reports through a
  `ProgressObserver` (additive on 015's worker); the cell joins the worker and
  `GET /events` on one process-wide bus (`aicortex::events()`), because
  `routes` and `services` are built apart from the chassis's state. The
  observer resolves the scope's owner with `ScopeRepo::by_id` (additive on
  012) in a task, so the drain never waits on it.
- **D-16 (2026-11-24, this spec, build).** FR-009's arrangement. The test
  runs the real `aicortex` binary in `required` identity mode against the
  fixture rauthy: `first-boot` and `migrate` as verbs; the fixture's API key
  written as the cell's admin token (as `supervise` does for the rauthy it
  starts); `custody_client` registering the cell's own client; and the local
  model activated before `serve` with `operator::activate_configured`, the
  function behind `POST /operator/embedding/activate`, because that route
  needs a browser session and the cell's origin here is rauthy's. Artifacts
  are a generated static model pinned by digest. "Opened no outbound socket"
  is asserted by sampling the process's sockets with `lsof` every 50 ms from
  before the capture until two seconds after the embedding event, requiring
  that every socket be loopback or a wildcard listener, plus the shipped
  manifest declaring no egress host. Sampling can miss a connection shorter
  than its interval; the kernel's egress denial and the empty ceiling are the
  structural guarantee, and this is the process-level witness.
- **D-17 (2026-11-24, this spec, build).** `search.rs` and `entities.rs` are
  claimed by this spec and extended by 018, 017, 025, 026, 040, and 056, and
  the governance gate refuses `complete` while a claimed unit is absent
  (I-004). They exist as documentation-only modules that mount no route, so
  D-3's staging is unchanged: the 404 for those paths stands until 018 and 017
  mount them. No claim moved and no other spec's edge changed.
- **D-18 (2026-11-25, dependency edge, the 017 manifest PR).**
  `aicortex-api` takes the workspace's `aicortex-graph`, so 017 can mount
  the `GET /entities` routes D-3 stages (017 D-9). The edge points downward
  (surfaces over understanding, spec 002). It lands on its own, ahead of the
  code, under policy isolation (spec 044 section 3.3). No requirement or
  acceptance criterion changes.

- **D-19 (2026-11-26, dependency edge, the search surface manifest commit).**
  `aicortex-api` takes `aicortex-embed` as a normal dependency (it was a
  dev-dependency), so `POST /search` can hold the query embedder 018 B-2's
  vector channel needs behind a seam of this crate (`search::QueryEmbedder`)
  and the cell can supply one. The edge points downward (surfaces over
  understanding, spec 002) and `aicortex-recall` already depends on it. It
  lands on its own, ahead of the code, under policy isolation (spec 044
  section 3.3). `Cargo.lock` does not change. No requirement or acceptance
  criterion changes.

- **D-22 (2026-11-26, dependency edge, the 026 manifest commit).**
  `aicortex-api` takes the workspace's `tracing` as a dev-dependency, so
  026's FR-006 can capture every log line and span a search produces under
  the fully assembled router. It is a test-only edge: no shipped source of
  this crate names the crate. It lands on its own, ahead of the code, under
  policy isolation (spec 044 section 3.3). No requirement or acceptance
  criterion changes.

## Status

- **2026-11-24.** `implementation: complete`. B-1 to B-10 and FR-001 to FR-009
  hold, with the D-3 staging (`POST /search` and the entity routes arrive with
  018 and 017) and the D-15 event scoping. AC-1: `cargo test -p aicortex-api
  --locked`. AC-2 and FR-009 were run against the pinned rauthy
  (`rauthy-patched` 0.36.2-patched.3, linux/arm64 locally, linux/amd64 in CI)
  on loopback: `aicortex-api` `tests/live.rs` registers a client, completes
  authorization code with PKCE as a person, captures, reads the memory back
  with `GET /memories/{id}` (until 018 restores "searches for it"), is refused
  `memory.delete` with `insufficient_scope`, and is refused a token for another
  audience; `apps/aicortex` `tests/serve_capture.rs` boots `aicortex serve`,
  captures, waits for the `embedding` event, and finds every sampled socket
  loopback. Client: reqwest 0.13 (rustls) with rahi 0.7.1 chassis crates.
  Remaining by design, not by gap: B-6 curator and review-queue events (034,
  023), `POST /promote` 501 until 023 (D-12), and the chassis limits of D-10.

## Verification

```verify:cli
cargo test -p aicortex-api --locked
```
