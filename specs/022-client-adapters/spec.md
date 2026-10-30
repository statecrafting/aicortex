---
id: "022-client-adapters"
title: "Client adapters: one capability matrix, four install paths, zero client code in the core"
status: approved
kind: "feature"
domain: "clients"
created: "2026-09-03"
authors: ["Bartek Kus"]
implementation: pending
risk: medium
wave: 2
depends_on:
  - "021-mcp-server"
establishes:
  - "clients/README.md"
  - "clients/matrix.toml"
  - "clients/claude-code/"
  - "clients/codex/"
  - "clients/cursor/"
  - "clients/antigravity/"
  - "crates/aicortex-api/src/clients.rs"
  - "crates/aicortex-api/tests/clients.rs"
extends:
  - { spec: "020-http-api-and-scopes", unit: "crates/aicortex-api/src/router.rs", nature: additive }
references:
  - { unit: { kind: file, path: "docs/design/00-lineage.md" }, role: context }
  - { unit: { kind: file, path: "specs/013-write-gate-and-redaction/spec.md" }, role: context }
summary: >
  Connecting is one URL, and everything a client needs beyond that is
  generated rather than written by hand. This spec owns the capability
  matrix: which clients support remote MCP, which support OAuth against it,
  and what each offers for automatic capture, which is the only place they
  genuinely differ. It ships per-client configuration snippets and the hook
  or notification payloads that turn a session into captured memories, and
  a route that serves the correct snippet for a client so setup is a copy
  rather than a document to follow.
---

# 022: Client adapters

## 1. Purpose

Provider agnosticism is a claim that has to be maintained. The core is
already client-agnostic because MCP is (002 §3), but automatic capture is
not: Claude Code has hooks, Codex has notifications, Cursor has its own
hook configuration, and Antigravity has its own. If that difference leaks
into the server it becomes four code paths; if it lives nowhere it becomes
four wiki pages that rot. It lives here, in one matrix and four directories
of generated configuration.

## 2. Territory

The `clients/` tree and one module in the API crate that serves its
contents. No other crate contains a client name.

## 3. Behavior

- **B-1 (the matrix).** `clients/matrix.toml` records, per client:
  transport support, OAuth support, dynamic registration support, whether
  it needs the compatibility pair of 021 B-7, its automatic-capture
  mechanism, and the minimum version each was verified at, with the
  verification date. An unverified row says so rather than implying it
  works.
- **B-2 (connect is a URL).** For every client the manual path is: add a
  remote MCP server with this URL, complete the browser sign-in, done. No
  key is pasted anywhere, because none exists (constitution VII).
- **B-3 (generated snippets).** Each `clients/<name>/` holds the
  configuration snippet for that client, a capture hook or notification
  handler, and a README with the exact steps. Snippets are generated from
  the matrix and the deployment's public URL by
  `GET /api/v1/clients/{name}` so a user copies a filled-in snippet rather
  than editing a placeholder.
- **B-4 (automatic capture; amended 2026-10-25, D-2).** A capture hook
  sends a candidate to `POST /capture` with the client as the source and
  the session as the external reference. It never sends file contents
  wholesale: it sends the durable items the session produced, in the
  classes B-8 names, which is what is worth remembering. Failures are silent to the user and visible
  in the client's own log; a memory service that breaks a coding session
  when it is down will be uninstalled.
- **B-5 (per-client identity).** Each client registers as its own OAuth
  client, so a token can be revoked for one client without touching the
  others, and the actor on captured memories names which client produced
  them.
- **B-6 (no scope for promotion).** Automatic capture requests
  `memory.write` and `memory.read` only. An agent-driven client is never
  granted `memory.promote` (019 B-4).
- **B-7 (verification harness).** A test drives the recorded protocol
  fixtures for each client in the matrix, so a claim in the matrix is
  backed by a fixture. Adding a client is a matrix row, a directory, and a
  fixture set.
- **B-8 (durable memory, not runtime state; added 2026-10-25, D-2).** A
  capture hook sends only durable classes, each with its 011 B-4 kind: a
  preference (`Preference`), a correction (`Correction`), a decision
  (`Decision`), a commitment someone made (`Task`), or a fact the user
  wants kept (`Fact`). It never sends runtime state: the session's own
  work in progress (steps taken or left in the work at hand), tool output,
  local file system paths, connection or server settings, credentials, or
  which plugins, servers, hooks, or skills are active
  (`gbrain://memory-boundaries`). A `Task` is a commitment to a future
  action that should outlive the session, never a report of the session's
  progress. A durable decision about a setup may be captured as a decision
  without the setup's settings or credentials. The hook filters by the
  structure the client exposes (event type, tool name, message role),
  never by asking a model. The gate remains the backstop for credentials
  (013 B-3, B-4, D-14); it does not classify the other runtime classes, so
  this rule lives here. Each client's README states the rule and the off
  switch.

## 4. Functional requirements

- **FR-001.** `GET /api/v1/clients/{name}` returns a snippet whose URL
  matches the deployment's configured public URL, for every client in the
  matrix.
- **FR-002.** Every matrix row has either a verification date and a fixture
  set, or an explicit `verified = false`.
- **FR-003.** A capture hook payload from each client validates against the
  capture DTO.
- **FR-004.** A hook whose request fails returns success to its client and
  records the failure locally, asserted against a server returning 503.
- **FR-005.** For each client, a recorded session fixture that contains
  one item of each durable class B-8 names (a preference, a correction, a
  decision, a commitment, and a fact the user asks to keep) and one item
  of each runtime class it excludes (a work-in-progress note, tool output, a
  local path, an MCP server configuration, a credential, and a plugin
  activation) produces exactly five capture payloads, one per durable
  item, each with its B-8 kind, and none for any runtime item.
- **FR-006.** Every client directory's README states the B-8 rule (durable
  classes only, never runtime state) and names the setting that turns
  automatic capture off, asserted by the clients test reading each README
  for both.

## 5. Acceptance criteria

- **AC-1.** `cargo test -p aicortex-api --locked --test clients` passes.
- **AC-2.** At least two clients are verified end to end against a running
  instance, with dates recorded in the matrix.

## 6. Out of scope

The protocol itself (021). Ingestion of a client's own transcript files,
which is a source adapter (033). Any client-specific behavior inside the
server.

## 7. Resolved decisions

- **D-1 (2026-09-03, this spec).** Snippets are served by the deployment
  rather than published as static documentation, because the one value a
  user must get right is the URL of their own instance, and a static
  document cannot know it.

- **D-2 (2026-10-25, amendment, pending owner review).** Added B-8,
  FR-005, and FR-006, and amended B-4 to defer to B-8: capture hooks send
  durable classes only and never runtime state. The observation is
  `gbrain://memory-boundaries` (`docs/design/00-lineage.md`). The rule
  sits here rather than in the gate because a hook sees the session's
  structure (which event, which tool, which role) and the gate sees only
  text; 013 D-14 records the gate's side of the same decision. B-4 used to
  say a hook sends "the decision, the correction, or the summary"; B-8
  widens the sent classes to the durable kinds of 011 B-4 by adding
  preferences (`Preference`), commitments (`Task`), and facts the user
  wants kept (`Fact`), drops the session summary, which is not one of
  those kinds, and makes the excluded runtime classes explicit; FR-005
  tests both sides, and FR-006 tests that each client's README states the
  rule and its off switch. B-4 now names B-8 as the one list of what is
  sent, so the two rules cannot drift. The read-side pointer hook observed
  alongside this is outside this amendment and is not specified here. The
  amendment adds behavior to an approved spec that no human has reviewed,
  so this spec returns to `status: draft` (constitution V).

## Verification

```verify:cli
cargo test -p aicortex-api --locked --test clients
```
