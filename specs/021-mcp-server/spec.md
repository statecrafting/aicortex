---
id: "021-mcp-server"
title: "The MCP server: streamable HTTP, a small tool surface, OAuth discovery that bootstraps itself"
status: approved
kind: "kernel"
domain: "protocol"
created: "2026-09-03"
authors: ["Bartek Kus"]
implementation: in-progress
risk: critical
wave: 2
depends_on:
  - "020-http-api-and-scopes"
establishes:
  - "crates/aicortex-mcp/Cargo.toml"
  - "crates/aicortex-mcp/src/lib.rs"
  - "crates/aicortex-mcp/src/transport.rs"
  - "crates/aicortex-mcp/src/session.rs"
  - "crates/aicortex-mcp/src/tools.rs"
  - "crates/aicortex-mcp/src/resources.rs"
  - "crates/aicortex-mcp/src/compat.rs"
  - "crates/aicortex-mcp/src/render.rs"
  - "crates/aicortex-mcp/tests/protocol.rs"
  - "crates/aicortex-mcp/tests/tools.rs"
  - "crates/aicortex-mcp/tests/support/mod.rs"
  - "crates/aicortex-mcp/tests/live.rs"
  - "crates/aicortex-mcp/testdata/sessions/"
extends:
  - { spec: "010-chassis-adoption-and-workspace", unit: "apps/aicortex/src/cell.rs", nature: additive }
  - { spec: "010-chassis-adoption-and-workspace", unit: "apps/aicortex/Cargo.toml", nature: additive }
  - { spec: "010-chassis-adoption-and-workspace", unit: "Cargo.toml", nature: additive }
  - { spec: "020-http-api-and-scopes", unit: "crates/aicortex-api/src/dto.rs", nature: additive }
  - { spec: "020-http-api-and-scopes", unit: "crates/aicortex-api/tests/common/mod.rs", nature: additive }
references:
  - { unit: { kind: file, path: "specs/019-untrusted-content-boundary/spec.md" }, role: constraint }
summary: >
  One remote MCP server over streamable HTTP, stateless per request, serving
  every client that speaks the protocol. Seven tools, no more: capture,
  search, fetch, list, update, forget, and relate. Authorization is the
  chassis's resource server, so an unauthenticated initialize answers the
  challenge that lets a client discover the authorization server and
  register itself without the user pasting anything. Tool results carry
  envelopes and the framing statement, and the tool descriptions themselves
  never contain recalled content.
---

# 021: The MCP server

## 1. Purpose

MCP is what makes this product provider agnostic: Claude Code, Codex,
Cursor, and Antigravity all speak it, so one server serves all of them and
the core carries no client-specific code (002 §3).

Two things make an MCP server work in practice rather than in a demo, and
both were learned from the predecessor's transport layer, which is the part
of it that was genuinely good: strict clients tear down a connection when a
protocol error arrives as an HTTP error status, and clients differ in the
`Accept` headers they send and the session semantics they expect. This spec
takes the lesson and not the code.

## 2. Territory

The `aicortex-mcp` crate, mounted by the cell at `/mcp`. It translates
between the protocol and the API crate's handlers; it contains no
authorization logic of its own and no SQL.

## 3. Behavior

- **B-1 (transport).** Streamable HTTP: a single endpoint accepting `POST`
  for requests and `GET` for the server-initiated stream, with responses
  either a JSON body or a declared SSE stream (`rahi://026`) depending on
  the request and the client's `Accept` header. Both `application/json` and
  `text/event-stream` are accepted, and a request offering only one is
  served in that one.
- **B-2 (stateless).** Each request is handled independently. Session state
  that the protocol requires is derived from the token and the request; no
  server-side session map holds product state between calls. A session
  header from a client is echoed where the protocol requires it and never
  used as an authorization input.
- **B-3 (protocol errors are protocol errors).** A JSON-RPC error is
  returned as a JSON-RPC error envelope with HTTP 200, including for
  authorization failures inside an established session, because strict
  clients treat a non-200 as a transport failure and drop the connection.
  The exception is the unauthenticated bootstrap of B-4, where the HTTP
  challenge is the point.
- **B-4 (authorization bootstrap).** A request with no credential answers
  HTTP 401 with `WWW-Authenticate: Bearer resource_metadata=...`
  (`rahi://025` B-6). The client fetches the protected resource metadata,
  discovers rauthy, registers dynamically if it must, completes
  authorization code with PKCE, and returns with a token. The user pastes
  a URL and nothing else: no key, no secret, no configuration file with a
  credential in it.
- **B-5 (the tools).** Exactly seven, each mapping to an API handler and a
  scope: `capture` (write), `search` (read), `fetch` (read), `list`
  (read), `update` (write), `forget` (delete), `relate` (read, entity
  neighborhood). A tool that would need `memory.promote` does not exist:
  promotion is a human act through the API or the review surface, never a
  tool an agent can call.
- **B-6 (tool results).** Results carry envelopes and the framing statement
  of 019 as structured content, with the trace summary attached as
  metadata. Tool descriptions and input schemas are static text authored in
  this crate and never interpolate stored content, so a stored memory can
  never become part of an instruction the client reads (019 B-3).
- **B-7 (compatibility).** `compat.rs` holds a `search` and `fetch` pair
  shaped for clients that require that specific pair, and any header or
  content-negotiation quirk a named client needs. Every entry cites the
  client and the observed behavior it works around, so the file is a
  documented list rather than accumulated folklore.
- **B-8 (resources).** MCP resources expose read-only scope summaries and
  the review queue count. A resource never exposes memory bodies, because
  some clients load resources into context automatically, which is an
  instruction position under 019 B-3.
- **B-9 (limits).** Tool inputs are validated against their schemas before
  any work, `search` k is capped, `capture` batch size is capped, and every
  tool inherits the token rate-limit group.

## 4. Functional requirements

- **FR-001.** `initialize`, `tools/list`, and `tools/call` succeed against
  the recorded session fixtures for each supported protocol revision.
- **FR-002.** An unauthenticated request answers 401 with a
  `resource_metadata` challenge; an authorization failure inside an
  established session answers HTTP 200 with a JSON-RPC error.
- **FR-003.** A client sending `Accept: application/json` only receives a
  JSON body; one sending only `text/event-stream` receives a stream.
- **FR-004.** No tool description or input schema contains any string read
  from the store, asserted by a test that captures a distinctive memory and
  greps the full `tools/list` output for it.
- **FR-005.** The 019 injection fixture appears in a `search` result only
  inside an envelope, preceded by the framing statement.
- **FR-006.** `tools/list` returns exactly seven tools, and a test fails if
  an eighth is added without amending this spec.

## 5. Acceptance criteria

- **AC-1.** `cargo test -p aicortex-mcp --locked` passes.
- **AC-2.** The wave 2 exit condition: a real MCP client completes the
  bootstrap of B-4 against a running instance with a rauthy binary,
  captures a memory, and retrieves it, recorded in the spec's Status note
  with the client and version used.

## 6. Out of scope

Per-client installation and automatic capture (022). Prompts and sampling,
which this server does not offer. Local stdio transport, refused by the
thesis.

## 7. Resolved decisions

- **D-1 (2026-09-03, this spec).** Seven tools, fixed, with the count
  asserted by a test. A memory server that grows a tool per capability
  ends up with a tool list too long for a client's context, which degrades
  every other tool the user has. New capability arrives as a parameter on
  an existing tool or not at all.
- **D-2 (2026-09-03, this spec).** No promotion tool. The entire value of
  the instruction grade is that a human decided; a tool would make the
  agent the decider by default, since it is the one holding the connection.

- **D-3 (2026-10-10, amendment, approved by the owner 2026-10-10).** Owner
  direction 2026-10-10 builds this spec right after 020, ahead of 016 to 018
  (020 D-3). It keeps its dependency on 020, because every tool maps to a
  020 handler and this spec extends 020's DTOs. The seven-tool surface of
  D-1 is fixed and stays fixed: `capture`, `fetch`, `list`, `update`, and
  `forget` are served by this build; `search` and `relate` are listed with
  their final schemas from the start, and until 018 and 017 mount their
  routes each answers a JSON-RPC error with a stable `not_yet_available`
  code (B-3), never a partial or unranked result. 018's and 017's build
  sessions replace those answers through `extends` edges. Adding or
  removing a tool later would break clients that cached the list, which is
  why the list does not wait.

- **D-4 (2026-11-24, this spec, build).** A tool call is one in-process request
  to the API's own router (`aicortex_api::router_with`), made with the
  `Principal` and `Bearer` the chassis's bearer layer already resolved. The
  route's scope gate (`RequireScope`) is therefore the one that decides, so
  this crate holds no authorization logic, no scope table that could drift
  from the routes (a test pins each tool to its scope), and no SQL. Rejected:
  calling the handlers as functions, which take extractors this crate cannot
  build, and a scope check of its own, which would be a second boundary. It
  adds `tower` (the `util` feature, for `oneshot`) to the workspace
  dependencies, which axum already locks, with the `extends` edge on 010's
  `Cargo.toml`.
- **D-5 (2026-11-24, this spec, build).** How an API answer becomes a
  protocol answer (B-3). A 401 or 403, a 400, and a 5xx other than 501 are
  protocol-level and answer a JSON-RPC error at HTTP 200: code `-32001` with
  `data.code` of `unauthorized` or `insufficient_scope` and the scope the
  challenge named, `-32602` for arguments the route refused, `-32603` for the
  cell failing. A route's own outcome (404, 409, 422 gate refusal, 501) is a
  tool result with `isError: true` carrying the problem document, which never
  holds content, so a model can read the reason code and correct itself.
  `-32002` is an unserved resource URI and `-32010` with `data.code`
  `not_yet_available` is D-3's answer for `search` and `relate`. The staged
  two answer without checking a scope: the answer says nothing a token could
  not learn from `tools/list`.
- **D-6 (2026-11-24, this spec, build).** Stateless details B-2 leaves open.
  The revision of a request is its `MCP-Protocol-Version` header, read as
  2025-03-26 when absent (the protocol's rule for older clients); a header
  naming a revision not spoken is a `-32600` error at HTTP 200 rather than
  the 400 the protocol suggests, because B-3 is explicit. Revisions spoken:
  2025-03-26, 2025-06-18, 2025-11-25; `initialize` answers the requested one
  or the newest. No session id is issued; a client's is echoed when it is a
  short visible token and read by nothing. JSON-RPC batches are refused,
  because 2025-06-18 removed them and a batch would pay the token's rate
  limit once for many calls (B-9). Notifications and client responses answer
  202. `GET` is the server-initiated stream and carries only the chassis's
  keep-alive and shutdown event, since this server initiates no message;
  `DELETE` is 405. `structuredContent` is sent from 2025-06-18; every
  revision gets the same document as a text block.
- **D-7 (2026-11-24, this spec, build).** `/mcp` is a service-callable bearer
  route, as `/api/v1` is (020 D-5): each tool needs exactly its API route's
  scope, and no tool promotes, so nothing here needs a person's token that
  the route behind it would not demand. A `POST` is answered as JSON when
  the client offers `application/json` (alone or with the stream, or no
  `Accept`, or a wildcard), as a one-event stream when it offers only
  `text/event-stream`, and 406 when it offers neither. Both `/mcp` and the
  compat path are declared streaming routes (`rahi://026`).
- **D-8 (2026-11-24, this spec, build).** The caps B-9 asks for: `capture`
  takes 1 to 20 candidates (lower than the API's 100, since a model's call is
  not an importer's batch), `search` `k` is 1 to 20 (default 10), `list`
  `limit` is 1 to 50, a query is at most 2,000 characters, an entity name 200.
  `capture` also takes an optional `idempotency_key` (the API's
  `Idempotency-Key`, 020 B-8) so a model's retry creates no second row.
  Validation is typed deserialization with unknown fields refused plus the
  range checks, and runs before any route is called.
- **D-9 (2026-11-24, this spec, build).** B-7 selection. The server is
  stateless, so it cannot remember a client's `initialize` to choose a shape
  for later calls. The search and fetch shape is therefore a second path,
  `/mcp/compat/search-fetch`, serving the same seven tools: a user pastes
  that URL into the client that needs it. Titles are `Memory <id>`, never
  stored text (019 B-6), and the whole framed response is the `text` member.
  Its `search` result shaping reads `items[].id` and is written against 020's
  listing shape; 018 confirms it when `search` is served. Only the one
  accommodation I can cite (OpenAI's documented requirement) is listed. The
  `Accept` tolerance of B-1 is read from the protocol's two forms and is not
  attributed to a named client.
- **D-10 (2026-11-24, this spec, build).** B-8's resources: counts from
  `GET /stats` only. `aicortex://scopes/personal`, and the templates
  `aicortex://scopes/project/{key}` and `aicortex://scopes/shared/{key}`,
  answer total, by-kind, and by-status counts; `aicortex://review-queue`
  answers the quarantined count of the personal scope, which is the review
  queue until 023 gives it a producer of its own. Reading needs
  `memory.read`, through the same route gate. Both are keyed by closed
  vocabularies, so no stored string can reach them.
- **D-11 (2026-11-24, this spec, build).** The protocol harness is spec 020's
  (`tests/common/mod.rs`, included by path), extended additively with a mount
  hook so the MCP router and its bearer declaration are served in the same
  composition as `serve`: an `extends` edge on that unit. The hook also lets a
  test state the composition a chassis would need for the anonymous
  `initialize` of B-4 (see the Status note).

- **D-12 (2026-11-26, build, 025 part 2).** D-3's staging ends for `search`:
  018 and 025 landed `POST /search`, so the tool is a translation to that
  route like the others, takes 025's `unit`, `window_radius`, and
  `token_budget` beside `query`, `k`, `scope`, and `kinds` (which becomes the
  route's `filter.kinds`), and answers the route's framed document with the
  trace in metadata (B-6). `relate` stays staged until 017's tool lands. The
  seven tools and the static descriptions (B-5, B-6) are unchanged.

## Status

- **2026-11-24.** `implementation: in-progress`. B-1 to B-9 and FR-001 to
  FR-006 are built and tested (AC-1: `cargo test -p aicortex-mcp --locked`),
  with D-3's staging: `search` and `relate` are listed with their final
  schemas and answer `not_yet_available` until 018 and 017. **AC-2 is not met**,
  and this is what was run.
  - Run: the pinned rauthy (`rauthy-patched` 0.36.2-patched.3, linux/arm64) from
    `scripts/live-rauthy.sh up`, spec 020's driver for dynamic registration and
    authorization code with PKCE as a person, the cell composed as `serve`
    composes it, and `cargo test -p aicortex-mcp --locked --test live`. Over
    HTTP with rauthy's token: `initialize`, `capture`, and the read back with
    `fetch` (018 has not landed `search`, 020 D-3) pass, `forget` is refused
    inside the session for want of `memory.delete`, and a token for another
    audience is a 401 `invalid_token`.
  - Real client: the official TypeScript SDK client,
    `@modelcontextprotocol/sdk` 1.32.2 (`scripts/mcp-sdk-client.mjs`, Node
    26.0.0), over streamable HTTP. Presented the rauthy token from its first
    request, it connected, listed the seven tools and the two resources,
    captured a memory and read it back framed. That is not the bootstrap of B-4.
  - Not met: the same client opening anonymously, which is where B-4 begins,
    ends at the first `POST`: `Streamable HTTP error: Error POSTing to endpoint:
    {"error":"csrf",...}`, HTTP 403 with no `WWW-Authenticate`. This is rahi
    0.7.1's edge, whose CSRF exemption for a declared bearer route requires an
    `Authorization` header (`rahi-cli` `serve.rs`), so a request with no
    credential is refused before the bearer layer can answer 401 (020 D-10, now
    pinned by `the_chassis_refuses_an_anonymous_post_before_the_challenge`). A
    `GET` is challenged correctly, and with the edge exempting declared bearer
    routes the handler answers 401 with `resource_metadata` on `POST` as well
    (`fr002_no_credential_is_the_discovery_challenge`). Needs a rahi change.
  - Also unproven, and not tested here: 020 D-14 records that the pinned
    rauthy cannot be asked for an audience by a dynamically registered client,
    so a client that registers itself would receive a token the chassis refuses
    on audience until rauthy or rahi supports it. AC-2 as worded (registration,
    authorization, capture, retrieval with nothing pasted but a URL) needs both.

- **2026-11-26.** `search` is served (D-12); only `relate` still answers
  `not_yet_available`. AC-3 of spec 025 (`cargo test -p aicortex-mcp --locked
  --test tools`) covers the tool's delivery parameters. The AC-2 gaps above are
  unchanged.

## Verification

```verify:cli
cargo test -p aicortex-mcp --locked
```
