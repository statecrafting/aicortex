---
id: "025-evidence-delivery-and-token-budget"
title: "Evidence delivery: a selectable evidence unit under a token budget that counts the framing"
status: draft
kind: "feature"
domain: "retrieval"
created: "2026-10-25"
authors: ["Bartek Kus"]
implementation: pending
risk: high
wave: 2
depends_on:
  - "018-retrieval-and-recall-trace"
  - "019-untrusted-content-boundary"
  - "020-http-api-and-scopes"
  - "021-mcp-server"
establishes:
  - "crates/aicortex-recall/src/delivery.rs"
  - "crates/aicortex-recall/tests/delivery.rs"
  - "crates/aicortex-recall/testdata/delivery/"
extends:
  - { spec: "018-retrieval-and-recall-trace", unit: "crates/aicortex-recall/src/lib.rs", nature: additive }
  - { spec: "019-untrusted-content-boundary", unit: "crates/aicortex-recall/src/envelope.rs", nature: additive }
  - { spec: "020-http-api-and-scopes", unit: "crates/aicortex-api/src/search.rs", nature: additive }
  - { spec: "020-http-api-and-scopes", unit: "crates/aicortex-api/src/dto.rs", nature: additive }
  - { spec: "020-http-api-and-scopes", unit: "crates/aicortex-api/tests/api.rs", nature: additive }
  - { spec: "021-mcp-server", unit: "crates/aicortex-mcp/src/tools.rs", nature: additive }
  - { spec: "021-mcp-server", unit: "crates/aicortex-mcp/tests/tools.rs", nature: additive }
  - { spec: "011-memory-model", unit: "crates/aicortex-types/src/provenance.rs", nature: additive }
  - { spec: "012-store-schema-and-repositories", unit: "crates/aicortex-store/src/migrations.rs", nature: additive }
  - { spec: "012-store-schema-and-repositories", unit: "crates/aicortex-store/src/provenance_repo.rs", nature: additive }
references:
  - { unit: { kind: file, path: "docs/design/00-lineage.md" }, role: context }
  - { unit: { kind: file, path: "specs/019-untrusted-content-boundary/spec.md" }, role: constraint }
  - { unit: { kind: file, path: "specs/031-conversation-and-archive-imports/spec.md" }, role: constraint }
summary: >
  Recall ranks memories and reduces each to its best chunk (018 B-4). For a
  question whose answer depends on a whole conversation, that chunk is too
  little, and a client that wants more must fetch each record in a second
  call with no budget at all. This spec adds a delivery stage after
  ranking: the caller selects an evidence unit (chunk, neighbor window,
  whole record, or auto), and the stage packs the ranked results into a
  token budget that is a hard cap, in the declared counter's units, over
  everything the model reads, including 019's framing statement, every
  envelope delimiter and label, and every cut marker. For a turn of an
  imported conversation, stored one memory per message under 031, the
  window is the turns around the hit and the record is the whole
  conversation. The default counter is a fixed formula over UTF-8 bytes,
  named in every response. Packing is deterministic. The default unit
  stays the chunk until the evaluation corpus (040) shows that another one
  is better.
---

# 025: Evidence delivery and the token budget

## 1. Purpose

A memory system is used by a model with a finite context, and the unit of
text it returns decides two things at once: whether the answer is in what
the model sees, and what that costs. gbrain measured the trade on a public
benchmark (`gbrain://evidence-unit-auto`): on 500 LongMemEval-S development
questions, returning the whole conversation for a hit inside a chat and the
ranked chunk otherwise answered 445 questions, against 312 for chunks only
and 457 for whole records, at about 15.1K reader tokens per question against
about 3.5K. Neighbor windows did markedly worse than whole sessions in an
earlier run. A preregistered sealed set of 150 questions then showed no
benefit and no harm, because its chats were short. The observation is that
the unit matters a great deal for conversational memory, that it costs real
tokens, and that a gain on development data is a hypothesis until a sealed
set confirms it.

This product has two gaps against that observation. 018 B-4 reduces a
memory to its best chunk with no alternative, and 018 B-9's budget is a
`Duration`, which bounds work but not what reaches the model. A client that
passes a context limit today cannot get a promise back.

This spec serves constitution X and XIV: the budget is computed over the
framed output 019 produces, so the boundary is never the part that gets
trimmed, and the default unit changes only with evaluation evidence.

## 2. Territory

A `delivery` module in `aicortex-recall` with its tests and fixtures. It
extends 018's public surface to export the stage, 019's envelope module so
an envelope can carry part of a body with its byte range and a cut marker,
020's search handler and DTOs with the delivery parameters, and 021's
`search` tool with the same parameters. It adds no table, no worker, no
route, and no tool. Its only possible schema change is the one described
next: if it is built before 031, its migration adds 031's nullable
conversation columns and their index to the existing provenance table.

The conversation units (B-1, B-2) read the conversation reference that
031 B-9 adds to provenance, through 031 B-11's `conversation_turns` read.
031 is approved and this spec is a draft, so either may be built first.
If this spec is built first, its build adds that field, its nullable
columns and index, and that read exactly as 031 B-9 and B-11 define them,
including B-9's wire form and B-11's `TurnRange`, through the `extends`
edges on 011's `provenance.rs` and 012's migrations and provenance
repository, and 031's build then uses them rather than adding them again;
031 FR-010's fixture then fails 031's build if the two definitions
diverge. Until a memory carries a conversation reference, the
conversation units are unreachable and every memory is delivered on its
own.

The stage runs after 018 has produced its fused order. It never changes
that order and never re-ranks. The only memories it delivers that ranking
did not return are the other turns of a returned memory's conversation,
inside that memory's `Window` or `Record` (B-1), read under B-9.

## 3. Behavior

### 3.1 The unit

- **B-1 (units).** `EvidenceUnit` is a closed enum:
  - `Chunk`: the winning chunk of 018 B-4, with its byte range. This is
    today's behavior.
  - `Window { radius }`: for a memory that is not conversational (B-2),
    the winning chunk plus `radius` neighboring chunks of the same memory
    on each side (1 to 3, default 1), merged where windows of the same
    memory overlap. For a conversational memory, the whole hit message
    plus the turns of its conversation numbered within `radius` of the
    hit's turn on each side, each as a whole message, with the same
    `radius` range (1 to 3, default 1) as above: 031 B-11's
    `conversation_turns` with `Between { first: turn - radius, last: turn
    + radius }`, with `first` clamped at turn 1. No upper clamp is needed:
    `Between` selects by turn number and returns only the turns that
    exist (031 B-11). A range past the conversation's last turn returns
    the turns up to the last one. A skipped or absent turn inside the range
    leaves a hole in what is returned; the range keeps its bounds and is
    never widened to make up the missing turn.
  - `Record`: for a memory that is not conversational, the whole memory
    body, which 013 B-6 bounds at 64 KiB. For a conversational memory, the
    whole conversation, rebuilt from the shared conversation id: every
    turn 031 B-11's `conversation_turns` returns with `All`, in turn
    order.
  - `Auto`: `Record` for a conversational memory, and `Chunk` for every
    other memory.
- **B-2 (conversation is declared, not inferred).** A memory is
  conversational exactly when its provenance carries a conversation
  reference (031 B-9). It is never inferred from the text. Its
  conversation is the set of memories that share its scope, source system,
  and conversation id (031 B-9); an imported conversation is stored one
  memory per message (031 D-2), so a conversation unit gathers several
  memories, and each keeps its own envelope (B-10).
- **B-3 (default unit).** A request that asks for delivery carries a
  `unit`, a `window_radius`, or a `token_budget`. A request that carries
  none of them does not run this stage at all, and no budget, default or
  otherwise, applies to it, so its response is byte-identical to one from
  a build without this stage. A request that asks for delivery without
  naming a unit gets `Chunk`. Changing the default requires an evaluation run
  (040) reported in the pull request, under the same rule as 018 B-10.

### 3.2 The budget

- **B-4 (what is counted).** `TokenBudget` is a hard cap, in the declared
  counter's units (B-5), on the model-facing payload: 019's framing statement and its version line, every
  envelope's opening and closing delimiters and labels, every delivered
  body text, and every cut and omission marker. Nothing the model reads in
  that payload is uncounted. The trace summary that 020 and 021 attach as
  metadata is not part of the payload; it is bounded by 018 B-5's summary
  form instead.
- **B-5 (the counter).** Tokens are counted by one declared, in-process
  counter that makes no outbound call. Its identifier and version travel
  in every response that carries delivery and in the persisted trace
  (018 B-6), so a reader can tell which units a budget was enforced in.
  - The default counter is `utf8-bytes-div3`, version 1: a text of `n`
    UTF-8 bytes counts as `ceil(n / 3)` tokens. It needs no vocabulary
    file, works offline, and gives the same answer on every machine. For
    ordinary prose it overestimates the tokens of common subword
    tokenizers; for text whose tokens average under three bytes (dense
    punctuation, some code, some scripts) it can underestimate them.
  - `utf8-bytes`, version 1, counts one token per UTF-8 byte. It never
    under-counts a byte-level or byte-fallback subword tokenizer, because
    every such token covers at least one byte, at the cost of delivering
    about a third of the text the default does for the same budget.
  - An operator who wants the count of a specific model configures that
    model's tokenizer as the counter, by identifier and version, with its
    vocabulary held locally. If a configured counter cannot run (its
    vocabulary is missing or fails to load), the stage counts with
    `utf8-bytes` and the response names that fallback.
  The budget is a hard cap in the declared counter's units. It is an
  exact token count only for a model whose own tokenizer is the declared
  counter; against any other model it is an estimate, and the response
  names the counter so the client can tell which.
- **B-6 (packing).** Packing walks the fused order once and is
  deterministic. Every reduction, for every rank, follows one ladder from
  the requested unit downwards: `Record`, then `Window { radius }` for
  each radius from the request's `window_radius` (default 1) down to 1,
  then `Chunk`. `Auto` is not a rung: before packing, it is resolved per
  memory under B-1 and B-2 to `Record` or `Chunk`, and that resolved unit
  is the requested unit for that memory's walk down the ladder. "Largest
  unit no larger than the requested one" means the first rung of that
  ladder, at or below the requested unit, that fits. For a conversational
  memory the rungs are the conversation units of B-1. A turn already
  delivered inside an earlier result's `Window` or `Record` is not
  delivered again: a later result that is such a turn is reported with
  the reason `covered` and the id of the result that carried it, uses no
  budget, and keeps its place in the prefix.
  1. The rank-one result is reserved first. If its requested unit does not
     fit, it is reduced along the ladder; if its chunk alone does not fit, it is cut at a sentence or line boundary and
     ends with a cut marker. It is never dropped.
  2. Each later result, in fused order, is delivered at the largest unit
     no larger than the requested one that fits in what is left, along the
     same ladder.
  3. The first result that does not fit even as its chunk is dropped, and
     so is every result after it, so the delivered set is always a prefix
     of the fused order. Dropped results are listed by id with the reason
     `budget`.
  4. Nothing is delivered outside the budget: no result spills past the
     cap as a smaller unit appended at the end.
  5. The final payload is serialized through 019's framing and recounted.
     If the recount exceeds the budget, the last delivered result is cut or
     dropped until it does not, except that the rank-one result is only
     ever cut, never dropped; B-7's minimum guarantees that its cut form
     fits. The reported `used` is that recount.
- **B-7 (minimum budget).** A budget smaller than the framing statement,
  one envelope's delimiters and labels, a cut marker, and one token of body
  is refused with a typed error naming the minimum, rather than returning an
  empty or unframed response.
- **B-8 (server ceiling).** The deployment configures a default budget and
  a maximum; the default applies only to a request that asks for delivery
  (B-3) without naming a budget. A request may lower the budget and may not exceed the maximum;
  a request above it is clamped and the response says so. The `Duration`
  budget of 018 B-9 is unchanged and independent.

### 3.3 Safety and determinism

- **B-9 (re-read under the caller's scope).** Neighbor chunks, whole
  bodies, and conversation turns are read under the same scope and status
  predicates as 018 B-7, in the caller's scope only. A window over chunks
  never crosses into another memory. A conversation unit crosses only into
  the other turns of the same conversation (B-2), read through 031 B-11's
  `conversation_turns`, which applies those predicates, so a quarantined,
  expired, or erased turn never reaches the payload by being adjacent to a
  returned one. A missing turn is skipped, not marked, and turn numbers
  are never renumbered.
- **B-10 (inside the envelope).** Every delivered text, whatever its unit,
  is emitted by 019's envelope function, with the memory id, trust class,
  origin, and the delivered byte ranges in the label. In a conversation
  unit each turn is its own memory and gets its own envelope, labelled
  with its own trust class and actor and with its conversation id and
  turn, so an assistant turn and a user turn delivered together keep
  their separate trust classes (031 B-10). A cut marker is part
  of the envelope's contents and cannot be confused with a delimiter
  (019 B-7).
- **B-11 (reported per result).** Each delivered result reports the unit
  requested, the unit delivered, the reason, its byte ranges, the ids and turns of any
  conversation turns it carries, and its token count. The response
  reports the budget, the tokens used, the counter's identifier and
  version (and the fallback when B-5's applies), and the dropped ids. The
  reasons are, in order of precedence:
  - `covered`: the result is a turn already delivered inside an earlier
    result's conversation unit (B-6);
  - `cut`: the result is delivered as a cut chunk with a cut marker
    (B-6 step 1 or 5);
  - `conversation`: the result is delivered as a conversation unit, a
    `Window` or `Record` over turns (B-1), whether requested directly or
    resolved from `Auto`;
  - `reduced`: the result is delivered at a lower rung of the ladder than
    the unit requested for it (B-6);
  - `fit`: the result is delivered at the unit requested for it.
  A conversational memory delivered as a smaller conversation unit than
  requested (a `Window` for a requested `Record`, or a narrower `Window`)
  reports `conversation`, and its delivered unit shows the reduction. One
  reduced all the way to its chunk is no longer delivered as a
  conversation unit, so it reports `reduced`, or `cut` if the chunk was
  cut.
- **B-12 (determinism).** For a fixed corpus, query, filter, weights,
  clock, unit, and budget, the payload is byte-identical across runs
  (018 B-8). The stage reads nothing but the fused order, the store under
  B-9, and its arguments.
- **B-13 (nothing persisted).** Expanded text is never cached or
  persisted. A persisted trace (018 B-6) records the units, ranges, and
  counts, never the text.

### 3.4 Surfaces

- **B-14 (HTTP and MCP).** `POST /search` and the MCP `search` tool accept
  optional `unit`, `window_radius`, and `token_budget` parameters. The MCP
  surface stays at seven tools (021 B-5), and the tool's description stays
  static text (021 B-6).

## 4. Functional requirements

- **FR-001.** A property test over generated result sets and budgets
  asserts that the recounted payload never exceeds the budget, and that
  the delivered set is a prefix of the fused order.
- **FR-002.** With a budget exactly equal to the framing statement plus one
  envelope and a short body, the body is delivered; with one token less, it
  is cut; below the minimum, the request is refused with the typed error.
- **FR-003.** A rank-one record larger than the budget is delivered cut,
  with the marker inside its envelope, and never dropped.
- **FR-004.** A fixture where a quarantined memory and an erased memory sit
  next to a returned memory in the same scope shows neither in any
  `Window` or `Record` payload.
- **FR-005.** 019 FR-001 holds for every unit: the hostile literal body
  appears only inside an envelope, preceded by the framing statement, and
  a body containing the delimiter sequence is escaped.
- **FR-006.** Two runs of the committed delivery fixtures produce
  byte-identical payloads; a request that asks for delivery without a unit
  is byte-identical to one naming `Chunk`; and a request with no delivery
  parameter is byte-identical to the response of a build without the stage,
  even under a tight configured default budget.
- **FR-007.** A test asserts the stage opens no socket and writes nothing.
- **FR-008.** Over a committed fixture conversation stored one memory per
  message (031 B-9), with user and assistant turns, one quarantined turn,
  one erased turn, and a same-id conversation in another scope: `Record`
  on a hit delivers every `Active` turn of the conversation in turn order,
  each in its own envelope with its own trust class, conversation id, and
  turn; `Window { radius: 1 }` delivers the hit and the turns numbered one
  above and one below it, where they are `Active`; and neither the quarantined nor the erased turn, nor any
  turn from the other scope, appears in any payload.
- **FR-009.** When two results of one query are turns of the same
  conversation, the second is reported `covered` by the first, appears in
  the payload once, uses no budget, and the delivered set is still a
  prefix of the fused order.
- **FR-010.** The default counter counts committed fixtures of known UTF-8
  byte lengths as `ceil(bytes / 3)`; every delivery response and every
  persisted trace names the counter's identifier and version; and with a
  configured counter whose vocabulary is missing, the response names the
  `utf8-bytes` fallback and the recounted payload is within the budget in
  that fallback's units.

## 5. Acceptance criteria

- **AC-1.** `cargo test -p aicortex-recall --locked --test delivery`
  passes.
- **AC-2.** `cargo test -p aicortex-api --locked --test api` passes with the
  delivery cases.
- **AC-3.** `cargo test -p aicortex-mcp --locked --test tools` passes with
  the delivery parameters and still counts seven tools.
- **AC-4.** The evaluation corpus (040) reports the `Chunk`, `Window`,
  `Record`, and `Auto` units side by side, with tokens delivered per query,
  before any change to B-3's default is proposed.

## 6. Out of scope

Ranking, which is 018's and is not changed here. Reranking, query
expansion, and excerpt selection by a model, which would be egress. Lexical
excerpt selection inside a record, which gbrain measured as worse than the
whole record and which this spec does not carry. Grouping memories into a
delivered unit by anything other than a declared conversation reference
(031 B-9), such as topic or time proximity. Shipping any model's
tokenizer as the default counter (D-6).

## 7. Resolved decisions

- **D-1 (2026-10-25, this spec).** A new spec rather than an amendment to
  018. 018 owns ranking and the trace, and its section 6 places how results
  are framed for a model outside its scope. A token budget that counts the
  framing is a packing stage over 019's output, with its own failure modes
  and its own tests, so it is a separate seam with its own spec. 018 B-4
  still decides which chunk wins; this spec decides how much text around it
  is delivered.
- **D-2 (2026-10-25, this spec).** The budget counts the framing and every
  label, which is stricter than the observed system: gbrain's cap counts
  titles and evidence text but not the surrounding metadata
  (`gbrain://token-budget-cap`). Here the framing is mandatory (019 B-2),
  so a budget that ignored it would let the boundary push the payload past
  the cap, and the only way to get back under would be to shorten the
  framing.
- **D-3 (2026-10-25, this spec).** The delivered set is a rank prefix, and
  the rank-one result is cut rather than dropped. A lower-ranked small
  result filling space a higher-ranked record could not use would make the
  delivered order depend on sizes rather than ranking, which 040 cannot
  attribute.
- **D-4 (2026-10-25, this spec).** The default stays `Chunk`. The observed
  gain is from another system on development data, and its own sealed set
  did not confirm it. Constitution XIV requires this corpus's evaluation to
  show it before a default changes.

- **D-5 (2026-10-30, owner decision, resolves Q-1).** Q-1 asked whether
  031 stores an imported conversation as one memory or one memory per
  message. The owner answered on 2026-10-30: "store one memory per
  message. Link the messages with a shared conversation id and a turn
  number in their provenance", and placed the decision in 031, where it is
  recorded as 031 D-2. This spec refers to it: B-2 reads 031's
  conversation reference instead of a list of source kinds, `Window` over
  a conversational memory returns the turns around the hit, and `Record`
  rebuilds the whole conversation from the shared id (B-1). Each turn
  keeps its own envelope and trust class (B-10), and a turn delivered with
  an earlier result is reported `covered` rather than delivered twice
  (B-6). Rejected: Q-1's earlier sketch of a separate conversation unit
  that gathers siblings by external reference, which the shared id makes
  unnecessary.
- **D-6 (2026-10-30, owner decision, resolves Q-2).** Q-2 asked which
  counter is the default. The owner answered on 2026-10-30: "default to a
  simple byte-based estimate, and name the counter in every response."
  Claude's tokenizer is not public and clients run many models, so no
  single exact counter fits all of them, and a hard cap can only be hard
  in the units of the counter it declares. The default is
  `utf8-bytes-div3`, `ceil(bytes / 3)`, the formula the owner proposed;
  the counter's identifier and version go in the response and the trace,
  and an operator may plug in a model's tokenizer (B-5). Reconciled with
  the earlier text, whose only counter was one token per UTF-8 byte: that
  counter is kept as `utf8-bytes`, a selectable conservative counter and
  the fallback when a configured counter cannot run, and it is not the
  default. The default is the divided form because a per-byte cap delivers
  about a third of the text the same budget would deliver under a subword
  tokenizer, starving the model of evidence, while the divided form
  overestimates for ordinary text, which is the owner's stated
  requirement. The cost, stated in B-5, is that the default can
  underestimate text whose tokens average under three bytes. The fallback
  for a failed configured counter is `utf8-bytes` rather than the default
  because an operator who configured a model's tokenizer asked for a cap
  that holds against that model, and only the per-byte count cannot
  under-count it. Rejected: a pinned tokenizer of one model family as the
  default, which needs vocabulary files and is exact for one family only.

## Verification

```verify:cli
cargo test -p aicortex-recall --locked --test delivery
cargo test -p aicortex-api --locked --test api
cargo test -p aicortex-mcp --locked --test tools
```
