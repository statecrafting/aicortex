---
id: "022-client-adapters"
title: "Client adapters: one capability matrix, four install paths, zero client code in the core"
status: approved
kind: "feature"
domain: "clients"
created: "2026-09-03"
authors: ["Bartek Kus"]
implementation: in-progress
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
  - "clients/capture-core.mjs"
  - "crates/aicortex-api/src/clients.rs"
  - "crates/aicortex-api/tests/clients.rs"
extends:
  - { spec: "020-http-api-and-scopes", unit: "crates/aicortex-api/src/router.rs", nature: additive }
  - { spec: "001-agentic-harness", unit: "spec-spine.toml", nature: additive }
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

- **D-3 (2026-11-26, build).** How the hook gets a token is not specified,
  and the spec's own rules leave it open: a hook is a process of its own, so
  it cannot borrow the client's OAuth session, and no static key may exist
  (constitution VII). The hook reads a bearer token from
  `AICORTEX_ACCESS_TOKEN` or a per-client file (`~/.aicortex/<client>.token`)
  and sends nothing, logging that it had none, when there is neither. How a
  person obtains a per-client token with `memory.read` and `memory.write`
  only, without pasting a key, needs a chassis flow (a device or loopback
  login for a non-MCP process) that rahi does not yet offer; it is not
  invented here. Until it exists the hooks are tested with minted tokens
  only.
- **D-4 (2026-11-26, build).** `GET /api/v1/clients/{name}` is gated by
  `memory.read` like the rest of the prefix, because `/api/v1` is declared a
  bearer route and a request with no credential is refused there. The URL it
  fills in is the deployment's `RAHI_PUBLIC_URL`, which a user already
  knows; serving the snippet before sign-in would need a public route outside
  the prefix and is not done.
- **D-5 (2026-11-26, build).** How a hook recognises a durable item
  structurally (B-8). A hook reads only user and assistant message text, and
  within it only lines that begin with one of five labels, each naming one
  011 B-4 kind: `Preference:`, `Correction:`, `Decision:`, `Commitment:`
  (`Task`), `Remember:` (`Fact`). Tool use and results, shell, MCP, plugin,
  system, and meta events are never read for content, even when their text
  carries a label. The labels are an authoring convention the user (or an
  instruction the user puts in the client's own memory file) follows; it is
  deterministic, and a line with no label is never sent. A smarter
  classifier would need a model, which B-8 forbids.
- **D-6 (2026-11-26, build).** Hooks are Node ES modules: one shared
  `clients/capture-core.mjs` (the filter, the request, the silent failure)
  and one small `hook.mjs` per client that maps that client's payload to
  session events. Node was chosen because the repository already ships Node
  tooling (`scripts/mcp-sdk-client.mjs`), `fetch` and `crypto` need no
  package, and a shell hook would need `jq`. The shared file is claimed in
  `establishes`. Each client's fixtures are `fixtures/invocations.json` (a
  list of `{stdin, args}` the test feeds the hook; `${FIXTURES}` expands to
  the fixtures directory) beside any payload file it names. They are
  authored in the shape each vendor documents, not recorded from a session:
  no client could be driven here, so no matrix row claims more than the
  fixtures show.
- **D-7 (2026-11-26, build).** The matrix is parsed by a reader for the
  subset of TOML the file uses (`[client.<name>]` tables of strings,
  booleans, and one-line string lists), because `aicortex-api` has no TOML
  dependency and the dependency set belongs to spec 010. The reader refuses
  anything outside the subset by line. The MCP path `/mcp` is repeated in
  `clients.rs` because `aicortex-mcp` depends on this crate; spec 021 owns
  the value. The matrix and templates are embedded with `include_str!`, so
  the served text is the repository's text; the one `match` that maps a row
  to its template is covered by a test that refuses a row with no template
  or a directory with no row.
- **D-8 (2026-11-26, build).** Every matrix row is `verified = false`. B-1's
  oauth and dynamic-registration columns record what each vendor documents,
  not what was observed against this server, and the compatibility pair of
  021 B-7 is `false` for all four because 021 cites a need for one client
  only (OpenAI connectors), which is not in the matrix. Antigravity's hook
  surface and MCP configuration key are unchecked, so its row says so and
  its handler reads a neutral event list.

- **D-9 (2026-11-26, build).** `spec-spine.toml [index] extra_hashed_inputs`
  gains `clients/matrix.toml`, `clients/README.md`, and
  `clients/capture-core.mjs`, because the lint (L-008) refuses a claimed file
  that is in no content hash and these three are the ones that change what
  every client is told. The hooks and snippets under the client directories
  are claimed as directories and stay out of the hash, as other source does.
  The edge on spec 001's `spec-spine.toml` is additive.

## Status

- **2026-11-26.** `implementation: in-progress`. Built and tested: B-1 to
  B-4, B-6, B-8, and FR-001 to FR-006 (AC-1:
  `cargo test -p aicortex-api --locked --test clients`, which drives each
  shipped hook with Node against its fixtures, once against a recording
  stub, once against a 503 and a dead port, and once against the real
  cell). **AC-2 is not met.** It needs two clients completing the browser
  sign-in against a running instance and a hook with a token of its own, and
  neither exists yet: spec 021's AC-2 is blocked on the rahi chassis (rahi
  specs 051 and 052, unreleased) refusing an anonymous `POST` before it can
  be challenged and on a dynamically registered client not receiving a token
  with this resource's audience (020 D-14), and the per-client token for a
  hook has no chassis flow (D-3 here). B-5's per-client OAuth registration is
  therefore designed in (each hook and snippet is per client, and the actor
  names the client) but unobserved. What can run today is a hook with a
  token minted for the test, which is what the tests do. When rahi lands the
  flow: sign in two clients, run their hooks, record versions and dates in
  `matrix.toml`, and flip those rows to `verified = true`.

## Verification

```verify:cli
cargo test -p aicortex-api --locked --test clients
```
