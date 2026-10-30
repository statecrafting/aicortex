---
id: "026-read-side-entity-pointers"
title: "Read-side entity pointers: tell the agent each turn that memory about a named entity exists"
status: draft
kind: "feature"
domain: "clients"
created: "2026-10-25"
authors: ["Bartek Kus"]
implementation: pending
risk: high
wave: 2
depends_on:
  - "017-entities-and-typed-edges"
  - "019-untrusted-content-boundary"
  - "020-http-api-and-scopes"
  - "021-mcp-server"
  - "022-client-adapters"
establishes:
  - "crates/aicortex-graph/src/pointer.rs"
  - "crates/aicortex-graph/tests/pointer.rs"
  - "crates/aicortex-graph/testdata/pointers/"
  - "crates/aicortex-api/src/pointers.rs"
  - "crates/aicortex-api/tests/pointers.rs"
extends:
  - { spec: "017-entities-and-typed-edges", unit: "crates/aicortex-graph/src/lib.rs", nature: additive }
  - { spec: "020-http-api-and-scopes", unit: "crates/aicortex-api/src/router.rs", nature: additive }
  - { spec: "020-http-api-and-scopes", unit: "crates/aicortex-api/src/dto.rs", nature: additive }
  - { spec: "021-mcp-server", unit: "crates/aicortex-mcp/tests/tools.rs", nature: additive }
  - { spec: "022-client-adapters", unit: "clients/matrix.toml", nature: additive }
  - { spec: "022-client-adapters", unit: "clients/README.md", nature: additive }
  - { spec: "022-client-adapters", unit: "clients/claude-code/", nature: additive }
  - { spec: "022-client-adapters", unit: "clients/codex/", nature: additive }
  - { spec: "022-client-adapters", unit: "clients/cursor/", nature: additive }
  - { spec: "022-client-adapters", unit: "clients/antigravity/", nature: additive }
  - { spec: "022-client-adapters", unit: "crates/aicortex-api/tests/clients.rs", nature: additive }
references:
  - { unit: { kind: file, path: "docs/design/00-lineage.md" }, role: context }
  - { unit: { kind: file, path: "specs/019-untrusted-content-boundary/spec.md" }, role: constraint }
summary: >
  Spec 022 turns a session into captured memories, but nothing on the read
  side tells an agent that the store already holds something about the
  person or project the user just named, so the agent answers generically
  about facts the store knew. This spec adds a per-turn pointer: a client
  hook sends the user's message to one read-only route, which resolves
  entity names by exact and normalized alias match in the caller's scope,
  with no model call, and returns a small framed block of entity ids,
  kinds, counts, and last-seen times. The block is untrusted data under
  019, bounded in size, scoped by the caller's token, opt-in per client,
  and the hook does nothing when it fails.
---

# 026: Read-side entity pointers

## 1. Purpose

gbrain observed that without a per-turn pointer, agents discuss an entity
for several messages without retrieving its record, even when a rich
record exists (`gbrain://retrieval-reflex`). Its remedy is a compact
pointer injected each turn for every resolvable entity in the user's
message, built with no model call and doing nothing when it fails. The
pointer does not answer anything; it tells the agent that retrieval would
be worth a call.

This product has the entities (017), a read surface (020, 021), and a
per-client hook mechanism (022), but 022 is capture only. This spec adds
the read-side half under the product's own rules: the pointer is
untrusted data (constitution X, 019), it reveals nothing outside the
caller's scope (constitution VII), and it contains no generated prose
(017 B-6), so gbrain's one-line page summary is replaced by
structured fields.

## 2. Territory

A `pointer` module in `aicortex-graph` (017) that resolves names in a text
against a scope's entities, with tests and fixtures. A `pointers` handler in
`aicortex-api` serving `POST /api/v1/pointers`, mounted through an
`extends` edge on 020's router and DTOs. Additive extensions of 022's
`clients/` tree: one capability row per client in `clients/matrix.toml`, a
pointer hook beside each client's capture hook, and the README steps. It
adds no table, no migration, no worker, and no MCP tool.

## 3. Behavior

### 3.1 Resolution

- **B-1 (resolve, never create).** `pointers(scope, text, bounds) ->
  PointerSet` finds entity names in `text` by 017 B-2's first two steps
  only: exact alias match, then normalized-form match, over entities that
  already exist in `scope`. It never uses embedding similarity, never
  calls a model, never creates an entity, and never records a mention. It
  is a pure function of the stored entities and its arguments.
- **B-2 (the message is not stored).** The text a hook sends is used for
  resolution and discarded. It is not persisted, not embedded, not logged,
  and not placed in a trace. The route is excluded from every request or
  response body logging layer the router mounts, so the body is not logged
  before the handler runs either. FR-006 tests this against the fully
  assembled router, so a layer added later that logged the body would fail
  that test. A pointer request is not a capture and
  does not pass through the write gate, because it writes nothing.
- **B-3 (live sources only).** An entity is pointed at only if at least
  one `Active` memory in the scope mentions it. Its mention count and last
  seen time are computed over `Active` memories only, so a pointer never
  reveals that erased or quarantined content existed.

### 3.2 The pointer

- **B-4 (contents).** A `Pointer` holds the entity id, its kind (017 B-1),
  the canonical name, the count of `Active` memories mentioning it, the
  capture time of the newest one, and the byte range of the match in the
  sent text. It holds no memory body, no edge object, no alias list, and no
  sentence derived from stored text.
- **B-5 (framed as data).** The canonical name is derived from stored
  content, so the set is returned under 019: the framing statement first,
  and each pointer inside an envelope labelled as a derived value with the
  entity's provenance (019 B-6). The only other text in the response is a
  fixed sentence authored in this crate that names the read tools (`fetch`,
  `relate`, `search`, as 021 B-5 names them) able to retrieve the entity. It never interpolates
  stored content and never tells the agent it must act.
- **B-6 (bounded).** A set holds at most a configured number of pointers
  (default 5) and at most a configured token size including the framing
  and every envelope label (default 400). Tokens are counted by one named,
  versioned, in-process counter that makes no outbound call, whose
  identifier travels in the response; the deployment configures which
  counter it is, and this spec takes no dependency on any other spec's
  counter. Where no counter is configured, size is counted in
  UTF-8 bytes, one token per byte, which never under-counts a subword
  tokenizer, and the response names that fallback. The bound is exact
  under the named counter and an estimate against a client model with a
  different tokenizer. The set is
  ordered by first match position in the text, then mention count
  descending, then entity id, and cut from the end, with the response
  marked truncated. The text a hook may send is bounded (default 8 KiB);
  longer text is resolved over its first bound only.
- **B-7 (scoped by the caller's token).** The route requires `memory.read`
  and resolves only in scopes the caller's token can read. An entity in
  another scope never produces a pointer, and a request naming a scope the
  caller cannot read answers as an empty set, indistinguishable from a
  scope with no matches (020's non-disclosing rule). The scope check runs
  before any lookup, is the same single check whether its answer is yes or
  no (and whether or not the named scope exists), and yields the entity set to resolve against: an
  unreadable scope yields the empty set, which then goes through the same
  resolution path as a readable scope that holds no entities. The
  guarantee is structural: the response and the code path are those of a
  readable empty scope, and nothing about an unreadable scope's contents
  is read, so its contents cannot shape the work done.

### 3.3 The client side

- **B-8 (capability row).** Each row of `clients/matrix.toml` gains a
  `read_pointer` entry: the client's per-turn hook mechanism or `none`,
  the position in which the client places that hook's output, whether that
  position is a data position, and the verification date or
  `verified = false`. A client whose only injection position is one it
  treats as directive (a system prompt, an instruction file, a rules file)
  is `none`, because 019 B-3 forbids placing stored content there. Until
  a human has verified from the client's documented semantics that its
  hook position is a data position, the row is `none` or `verified =
  false`, and no pointer hook snippet is generated for it, so an
  unverified client can never receive a pointer.
- **B-9 (opt-in).** The pointer hook is shipped beside the capture hook
  but enabled only when the user installs it from the generated snippet.
  A deployment can disable the route, which the hook treats as a failure.
- **B-10 (fail open).** The hook has a short deadline (default 300 ms). On
  any failure (no network, a non-2xx answer, a timeout, a malformed body)
  it adds nothing to the turn, returns success to its client, and records
  the failure in the client's own log, as 022 B-4 requires of capture.
- **B-11 (no instruction, no promotion).** A pointer grants no trust and
  changes no state. Appearing in a pointer does not make an entity or a
  memory instruction grade (019 B-4), and the hook requests `memory.read`
  only.

## 4. Functional requirements

- **FR-001.** Over the committed pointer fixtures, a message naming two
  known entities by alias and one unknown name yields exactly the two
  recorded pointers, in the recorded order, byte for byte.
- **FR-002.** A fixture entity whose only mentioning memory is erased, and
  another whose only one is quarantined, produce no pointer.
- **FR-003.** A cross-scope request returns the same response as a request
  against a scope with no matches, and an instrumented store records no
  read of the unreadable scope's entities, aliases, or mentions during it.
- **FR-004.** A hostile canonical name (019's delimiter and instruction
  fixtures) appears only inside an envelope, after the framing statement.
- **FR-005.** A message long enough to resolve more pointers than the
  bounds returns a truncated set whose counted size is within the token
  bound.
- **FR-006.** A test drives the fully assembled router, with every layer
  it mounts in production, under a capturing tracing subscriber at the
  most verbose level, and sends a unique sentinel string. It asserts the
  route writes nothing and opens no outbound network connection (the
  in-process store is the only thing it reads), and that the sentinel appears
  in no captured log line or span, no trace, and no stored row.
- **FR-007.** For each client with `read_pointer` enabled, the hook
  returns success and adds nothing when the server answers 503 or exceeds
  the deadline.
- **FR-008.** Every matrix row has a `read_pointer` entry that is either
  `none`, verified with a date and a fixture, or `verified = false`, and
  no pointer hook snippet is generated for a row that is `none` or
  `verified = false`.
- **FR-009.** A message longer than the text bound (8 KiB by default) is
  answered with success, not a client error, resolves names in its first
  bound only, and a name lying wholly past the bound produces no pointer.
- **FR-010.** A test in `aicortex-mcp`'s tool tests, which may depend on
  `aicortex-api` because the dependency points downward, asserts that
  every tool name in B-5's fixed sentence is a read tool in 021's tool
  list, so a rename there fails here.

## 5. Acceptance criteria

- **AC-1.** `cargo test -p aicortex-graph --locked --test pointer` passes.
- **AC-2.** `cargo test -p aicortex-api --locked --test pointers` passes.
- **AC-3.** `cargo test -p aicortex-api --locked --test clients` passes with
  the `read_pointer` rows and hook fixtures.
- **AC-4.** `cargo test -p aicortex-mcp --locked --test tools` passes with
  FR-010's tool-name check.

## 6. Out of scope

Retrieval itself, which the agent does through the existing tools. A
policy text that teaches the agent when to retrieve, which gbrain ships as
an installable skill; a client's own instructions are the user's to write,
and this repository does not author directive text for a client. Fuzzy or
embedding-based name resolution, which would need a model call per turn.
Entity summaries in prose, which 017 B-6 forbids.

## 7. Resolved decisions

- **D-1 (2026-10-25, this spec).** A new spec rather than an amendment to
  022. The pointer depends on 017's entities and on 019's framing, and its
  route and resolver are server behavior; 022 is the client tree and stays
  client-only, which its own section 2 requires ("no other crate contains a
  client name"). This spec reaches 022's units through `extends` edges, so
  022's requirements and acceptance are unchanged.
- **D-2 (2026-10-25, this spec).** Exact and normalized alias matching
  only. It costs a lookup per turn, needs no model, and is deterministic.
  The cost is that a paraphrase or a misspelling produces no pointer, which
  is the safe failure: a missing pointer leaves the agent where it is
  today.
- **D-3 (2026-10-25, this spec).** Off by default per client, unlike the
  observed system, where the pointer layer is on unless disabled. Every
  optional behavior in this product that reaches a model's context or
  leaves the process is opt-in, as a matter of this product's design
  rather than of any one spec, and this one sends the user's message to
  the server on every turn.

Open for the owner before approval:

- **Q-1.** Which current client hook outputs are data positions under
  019 B-3. A hook that adds context to the next model turn may be treated
  by some clients as a system-level note. This spec makes the matrix
  record the answer per client, but the first rows need a human judgment
  of each client's documented semantics. B-8 keeps an unanswered row
  safe in the meantime: it ships no pointer hook.

## Verification

```verify:cli
cargo test -p aicortex-graph --locked --test pointer
cargo test -p aicortex-api --locked --test pointers
cargo test -p aicortex-api --locked --test clients
cargo test -p aicortex-mcp --locked --test tools
```
