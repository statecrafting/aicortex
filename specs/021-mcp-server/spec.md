---
id: "021-mcp-server"
title: "The MCP server: streamable HTTP, a small tool surface, OAuth discovery that bootstraps itself"
status: approved
kind: "kernel"
domain: "protocol"
created: "2026-09-03"
authors: ["Bartek Kus"]
implementation: pending
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
  - "crates/aicortex-mcp/testdata/sessions/"
extends:
  - { spec: "010-chassis-adoption-and-workspace", unit: "apps/aicortex/src/cell.rs", nature: additive }
  - { spec: "020-http-api-and-scopes", unit: "crates/aicortex-api/src/dto.rs", nature: additive }
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

## Verification

```verify:cli
cargo test -p aicortex-mcp --locked
```
