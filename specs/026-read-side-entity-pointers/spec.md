---
id: "026-read-side-entity-pointers"
title: "Read-side entity pointers: tell the agent, inside its recall results, that memory about a named entity exists"
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
  - "025-evidence-delivery-and-token-budget"
establishes:
  - "crates/aicortex-graph/src/pointer.rs"
  - "crates/aicortex-graph/tests/pointer.rs"
  - "crates/aicortex-graph/testdata/pointers/"
  - "crates/aicortex-api/src/pointers.rs"
  - "crates/aicortex-api/tests/pointers.rs"
extends:
  - { spec: "017-entities-and-typed-edges", unit: "crates/aicortex-graph/src/lib.rs", nature: additive }
  - { spec: "020-http-api-and-scopes", unit: "crates/aicortex-api/src/search.rs", nature: additive }
  - { spec: "020-http-api-and-scopes", unit: "crates/aicortex-api/src/dto.rs", nature: additive }
  - { spec: "021-mcp-server", unit: "crates/aicortex-mcp/src/tools.rs", nature: additive }
  - { spec: "021-mcp-server", unit: "crates/aicortex-mcp/tests/tools.rs", nature: additive }
  - { spec: "022-client-adapters", unit: "clients/matrix.toml", nature: additive }
  - { spec: "022-client-adapters", unit: "clients/README.md", nature: additive }
  - { spec: "022-client-adapters", unit: "crates/aicortex-api/tests/clients.rs", nature: additive }
  - { spec: "025-evidence-delivery-and-token-budget", unit: "crates/aicortex-recall/src/delivery.rs", nature: additive }
references:
  - { unit: { kind: file, path: "docs/design/00-lineage.md" }, role: context }
  - { unit: { kind: file, path: "specs/019-untrusted-content-boundary/spec.md" }, role: constraint }
summary: >
  Spec 022 turns a session into captured memories, but nothing on the read
  side tells an agent that the store already holds something about the
  person or project it just asked about, so the agent answers generically
  about facts the store knew. This spec adds entity pointers to the
  existing recall result: when a search asks for them, the server resolves
  entity names in the query by exact and normalized alias match in the
  searched scope, with no model call, and returns a small framed block of
  entity ids, kinds, counts, and last-seen times beside the evidence. The
  block is untrusted data under 019, bounded in size, counted inside any
  delivery budget, and opt-in per request. No client hook carries pointers:
  every client's hook output starts as "not a data position", and the MCP
  surface stays at seven tools.
---

# 026: Read-side entity pointers

## 1. Purpose

gbrain observed that without a pointer, agents discuss an entity for
several messages without retrieving its record, even when a rich record
exists (`gbrain://retrieval-reflex`). Its remedy is a compact pointer
injected each turn for every resolvable entity in the user's message,
built with no model call and doing nothing when it fails. The pointer does
not answer anything; it tells the agent that retrieval would be worth a
call.

This product has the entities (017) and a read surface (020, 021). This
spec adds the pointer under the product's own rules: the pointer is
untrusted data (constitution X, 019), it reveals nothing outside the
caller's scope (constitution VII), and it contains no generated prose
(017 B-6), so gbrain's one-line page summary is replaced by structured
fields.

It does not carry gbrain's delivery channel. A per-turn hook's output
usually lands where the model treats it as instruction: Claude Code's
harness tells the model to treat hook output as feedback from the user,
which is a directive position under 019 B-3 and fails constitution X
(D-4). Pointers therefore travel inside the existing `search` tool's
result, which 019 already frames, so they are something the model asks
for rather than something pushed into every turn.

## 2. Territory

A `pointer` module in `aicortex-graph` (017) that resolves names in a text
against a scope's entities, with tests and fixtures. A `pointers` module in
`aicortex-api` that builds the framed pointer block and attaches it to the
search response, reached through `extends` edges on 020's search handler
and DTOs and on 021's `search` tool, whose input schema gains the request
flag. An `extends` edge on 025's delivery module, so the block is counted
inside a delivery budget. Additive extensions of 022's client tree: one
`read_pointer` entry per row of `clients/matrix.toml`, the README text that
explains it, and the clients test that checks it. It adds no table, no
migration, no worker, no route, no MCP tool, and no client hook.

## 3. Behavior

### 3.1 Resolution

- **B-1 (resolve, never create).** `pointers(scope, text, bounds) ->
  PointerSet` finds entity names in `text` by 017 B-2's first two steps
  only: exact alias match, then normalized-form match, over entities that
  already exist in `scope`. It never uses embedding similarity, never
  calls a model, never creates an entity, and never records a mention. It
  is a pure function of the stored entities and its arguments. The text it
  resolves over is the search request's query text.
- **B-2 (nothing new is stored).** The pointer stage persists nothing,
  embeds nothing, and adds nothing to the trace beyond the count of
  pointers returned. It logs no part of the query text. The query text is
  handled exactly as 018 and 040 B-11 already handle a search query:
  asking for pointers changes nothing about what is stored, traced, or
  logged. FR-006 tests this against the fully assembled router, so a layer
  added later that logged more for a pointer request would fail that test.
  A pointer request is not a capture and does not pass through the write
  gate, because it writes nothing.
- **B-3 (live sources only).** An entity is pointed at only if at least
  one `Active` memory in the scope mentions it. Its mention count and last
  seen time are computed over `Active` memories only, so a pointer never
  reveals that erased or quarantined content existed.

### 3.2 The pointer

- **B-4 (contents).** A `Pointer` holds the entity id, its kind (017 B-1),
  the canonical name, the count of `Active` memories mentioning it, the
  capture time of the newest one, and the byte range of the match in the
  query text. It holds no memory body, no edge object, no alias list, and
  no sentence derived from stored text.
- **B-5 (framed as data).** The canonical name is derived from stored
  content, so the set is returned under 019: it sits after the response's
  framing statement, which the response carries once for its memory
  envelopes and its pointers together, and each pointer is inside an
  envelope labelled as a derived value with the entity's provenance
  (019 B-6). The only other text in the block is a fixed sentence, a
  public constant of `aicortex-api`'s `pointers` module, that names the
  read tools (`fetch`, `relate`, `search`, as 021 B-5 names them) able to
  retrieve the entity. It never interpolates stored content and never
  tells the agent it must act.
- **B-6 (bounded).** A set holds at most a configured number of pointers
  (default 5) and at most a configured token size, including the fixed
  sentence and every envelope label (default 400). Tokens are counted by
  the deployment's declared counter of 025 B-5, so the bound is a hard cap
  in that counter's units. The block names the counter's identifier and
  version itself, because a search that asks for pointers without asking
  for delivery carries no other counter field (025 B-3). The set is ordered
  by first match position in the text, then mention count descending, then
  entity id, and cut from the end, with the block marked truncated. The
  text resolved over is bounded (default 8 KiB); a longer query is cut at
  the last UTF-8 character boundary within the bound and resolved over
  that prefix only, so a name that starts inside the bound and ends past
  it is not matched, and the block marks the text as truncated, separately
  from a truncated pointer set. The search itself still runs over the
  whole query.
- **B-7 (scoped by the search).** Pointers are resolved only in the scope
  the search reads, after 020's scope check has admitted the search, and
  under the same `memory.read` requirement. A search refused by that check
  returns no pointer block. An entity in another scope never produces a
  pointer, and nothing about another scope's entities, aliases, or
  mentions is read.

### 3.3 Delivery

- **B-8 (opt-in per request, inside the recall result).** `POST /search`
  and the MCP `search` tool accept an optional `pointers` flag, default
  off. A request without it does not run this stage, and its response is
  byte-identical to one from a build without this stage. A request with
  it receives the pointer block in a sibling field of the evidence, in the
  same result. A deployment can disable pointers; a request that asks for
  them then receives no block and a field saying pointers are disabled.
  The MCP surface stays at seven tools (021 B-5), and the `search` tool's
  description stays static text (021 B-6).
- **B-9 (inside the delivery budget).** When the same request carries a
  025 delivery budget, the pointer block is part of the model-facing
  payload (025 B-4). 025 packs the evidence first, exactly as it would
  without pointers; the pointer block is then fitted into what the
  evidence leaves, cut from the end as B-6 cuts it, down to no pointers at
  all, and marked truncated. The final recount of 025 B-6 step 5 covers
  the whole payload, pointers included; if it exceeds the budget,
  pointers are cut from the end first, and only once none remain may that
  step cut or drop a result. Pointers therefore never reduce, cut, or drop
  a delivered result, and the reported `used` includes them.
- **B-10 (no instruction, no promotion).** A pointer grants no trust and
  changes no state. Appearing in a pointer does not make an entity or a
  memory instruction grade (019 B-4).

### 3.4 The client side

- **B-11 (capability row, not a data position by default).** Each row of
  `clients/matrix.toml` gains a `read_pointer` entry: the client's per-turn
  hook mechanism or `none`, the position in which the client places that
  hook's output, `data_position`, and the evidence for it. `data_position`
  starts `false` for every client. It becomes `true` only when a test in
  `crates/aicortex-api/tests/clients.rs` shows that the client labels the
  text a hook injects as untrusted, and the row then names that test. A
  documented behavior, a vendor statement, or a human reading of the docs
  is not enough on its own. The Claude Code row records that its harness
  presents hook output to the model as feedback from the user, which is a
  directive position (019 B-3).
- **B-12 (no hook delivery).** This spec ships no pointer hook and no
  per-turn route for any client, whatever its row says. Pointers reach a
  model only inside the recall result (B-8). A row whose `data_position`
  becomes `true` records a fact about that client; delivering pointers
  through its hook is a later amendment, which must specify the hook, its
  deadline and failure behavior, and what sending each message to the
  server discloses.

## 4. Functional requirements

- **FR-001.** Over the committed pointer fixtures, a query naming two
  known entities by alias and one unknown name yields exactly the two
  recorded pointers, in the recorded order, byte for byte.
- **FR-002.** A fixture entity whose only mentioning memory is erased, and
  another whose only one is quarantined, produce no pointer.
- **FR-003.** A search in one scope whose query names an entity that
  exists only in another scope returns no pointer for it, and an
  instrumented store records no read of the other scope's entities,
  aliases, or mentions during it.
- **FR-004.** A hostile canonical name (019's delimiter and instruction
  fixtures) appears only inside an envelope, after the framing statement,
  in both the HTTP search response and the MCP `search` result.
- **FR-005.** A query long enough to resolve more pointers than the
  bounds returns a truncated set whose counted size, under the declared
  counter, is within the token bound.
- **FR-006.** A test drives the fully assembled router, with every layer
  it mounts in production, under a capturing tracing subscriber at the
  most verbose level, and sends the same search with a unique sentinel in
  its query twice, once with `pointers` and once without. The two runs
  write the same rows, open no outbound network connection, and produce
  the same captured log lines, spans, and persisted trace except for the
  pointer count, so the pointer stage adds no occurrence of the sentinel
  anywhere.
- **FR-007.** A search without `pointers` is byte-identical to the
  response of a build without this stage; with pointers disabled by the
  deployment, a search with `pointers` carries no block and says so.
- **FR-008.** Every matrix row has a `read_pointer` entry; every row whose
  `data_position` is `true` names a test in `tests/clients.rs` that exists
  and asserts the client labels injected text untrusted, and every other
  row is `false`; the Claude Code row is `false`; and no pointer hook
  snippet is generated for any client.
- **FR-009.** A query longer than the text bound (8 KiB by default) is
  answered with success, resolves names in its first bound only, marks the
  text as truncated, and a name lying wholly past the bound, or one that
  straddles it, produces no pointer.
- **FR-010.** A test in `aicortex-mcp`'s tool tests reads B-5's constant
  from `aicortex-api` and asserts that every tool name in it is a read
  tool in 021's tool list, so a rename there fails here. `aicortex-mcp`
  already depends on `aicortex-api` (021 maps every tool to an API
  handler), and the workspace layering forbids the reverse edge. The same
  tests assert `tools/list` still returns seven tools.
- **FR-011.** With a 025 budget, a search with `pointers` delivers exactly
  the same results as the same search without it, its recounted payload,
  pointers included, is within the budget, and at a budget the evidence
  nearly fills the block is cut, down to no pointers, and marked
  truncated.

## 5. Acceptance criteria

- **AC-1.** `cargo test -p aicortex-graph --locked --test pointer` passes.
- **AC-2.** `cargo test -p aicortex-api --locked --test pointers` passes.
- **AC-3.** `cargo test -p aicortex-api --locked --test clients` passes with
  the `read_pointer` rows.
- **AC-4.** `cargo test -p aicortex-mcp --locked --test tools` passes with
  the `pointers` flag, FR-010's tool-name check, and seven tools.

## 6. Out of scope

Retrieval itself, which the agent does through the existing tools.
Delivering pointers through any client's hook, or through a per-turn
route, which B-12 leaves to a later amendment. A policy text that teaches
the agent when to retrieve, which gbrain ships as an installable skill; a
client's own instructions are the user's to write, and this repository
does not author directive text for a client. Fuzzy or embedding-based name
resolution, which would need a model call per query. Entity summaries in
prose, which 017 B-6 forbids.

## 7. Resolved decisions

- **D-1 (2026-10-25, this spec).** A new spec rather than an amendment to
  022. The pointer depends on 017's entities and on 019's framing, and its
  resolver and the block it returns are server behavior; 022 is the client
  tree and stays client-only, which its own section 2 requires ("no other
  crate contains a client name"). This spec reaches 022's units through
  `extends` edges, so 022's requirements and acceptance are unchanged.
- **D-2 (2026-10-25, this spec).** Exact and normalized alias matching
  only. It costs a lookup per query, needs no model, and is deterministic.
  The cost is that a paraphrase or a misspelling produces no pointer, which
  is the safe failure: a missing pointer leaves the agent where it is
  today.
- **D-3 (2026-10-25, this spec; amended 2026-10-30 by D-4).** Off by
  default, unlike the observed system, where the pointer layer is on
  unless disabled. Every optional behavior in this product that reaches a
  model's context or leaves the process is opt-in, as a matter of this
  product's design rather than of any one spec. The opt-in was per client
  while the pointer travelled by hook; under D-4 it is per request (B-8).
- **D-4 (2026-10-30, owner decision, resolves Q-1).** Q-1 asked which
  current client hook outputs are data positions under 019 B-3. The owner
  answered on 2026-10-30: "treat no client's hook output as data for now,
  and deliver pointers through MCP tool results." The owner's reasons,
  which this revision carries: hook output usually lands where the model
  treats it as instructions, and Claude Code is the clearest case, because
  its harness tells the model to treat hook output as user feedback, so it
  fails constitution X; every `read_pointer` row therefore starts at "not a
  data position" and flips only when a test in `tests/clients.rs` shows
  that the client labels the injected text as untrusted (B-11); until then
  pointers are returned inside the existing recall tool's result, already
  framed under 019, which keeps the count at seven tools and makes
  pointers something the model asks for rather than something pushed into
  every turn (B-8); and this removes the 300 ms hook deadline and most of
  the privacy concern of sending every message to the server. Removed by
  this revision: the `POST /api/v1/pointers` route, the per-client pointer
  hooks and their `extends` edges on `clients/<name>/`, the hook deadline
  and fail-open behavior (former B-10 and FR-007), and the rule that a
  sent message is discarded unlogged (former B-2), which no longer has a
  message to govern. Rejected: keeping the hooks behind a human reading of
  each client's documentation, which Q-1 had proposed.
- **D-5 (2026-10-30, this spec).** Choices D-4 left open, recorded for the
  owner's review at approval. First, the recall result means both the MCP
  `search` tool and `POST /search`, because 021 maps the tool to that
  handler, and the flag is per request and off by default (D-3), so an
  existing search is unchanged byte for byte, as 025 B-3 keeps it.
  Second, because pointers now share a payload with 025's evidence, this
  spec depends on 025: the pointer bound uses 025's declared counter, and
  the block is fitted into what the evidence leaves (B-9), so a pointer
  never displaces evidence and a budget stays a hard cap over everything
  the model reads. Rejected: reserving pointer space before packing, which
  would let a list of entity names cut the rank-one result; and a separate
  counter for pointers, which would put two units in one response.

## Verification

```verify:cli
cargo test -p aicortex-graph --locked --test pointer
cargo test -p aicortex-api --locked --test pointers
cargo test -p aicortex-api --locked --test clients
cargo test -p aicortex-mcp --locked --test tools
```
