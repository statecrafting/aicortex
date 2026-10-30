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
references:
  - { unit: { kind: file, path: "docs/design/00-lineage.md" }, role: context }
  - { unit: { kind: file, path: "specs/019-untrusted-content-boundary/spec.md" }, role: constraint }
summary: >
  Recall ranks memories and reduces each to its best chunk (018 B-4). For a
  question whose answer depends on a whole conversation, that chunk is too
  little, and a client that wants more must fetch each record in a second
  call with no budget at all. This spec adds a delivery stage after
  ranking: the caller selects an evidence unit (chunk, neighbor window,
  whole record, or auto), and the stage packs the ranked results into a
  token budget that is a hard cap over everything the model reads,
  including 019's framing statement, every envelope delimiter and label,
  and every cut marker. Packing is deterministic. The default unit stays
  the chunk until the evaluation corpus (040) shows that another one is
  better.
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
`search` tool with the same parameters. It adds no table, no migration, no
worker, no route, and no tool.

The stage runs after 018 has produced its fused order. It never changes
that order, never re-ranks, and never adds a memory that ranking did not
return.

## 3. Behavior

### 3.1 The unit

- **B-1 (units).** `EvidenceUnit` is a closed enum:
  - `Chunk`: the winning chunk of 018 B-4, with its byte range. This is
    today's behavior.
  - `Window { radius }`: the winning chunk plus `radius` neighboring chunks
    of the same memory on each side (1 to 3, default 1), merged where
    windows of the same memory overlap.
  - `Record`: the whole memory body, which 013 B-6 bounds at 64 KiB.
  - `Auto`: `Record` for a memory whose provenance marks it as part of a
    conversation (B-2), and `Chunk` for every other memory.
- **B-2 (conversation is declared, not inferred).** Whether a memory is
  conversational is read from the provenance fields 011 defines (source
  kind and external reference), against a closed list of source kinds held
  in this module. It is never inferred from the text. The list starts with
  the conversational source kinds that exist when this spec is built; a
  later spec that introduces one (031's chat imports, for example) adds its
  kind to the list through an `extends` edge on this module.
- **B-3 (default unit).** With no unit requested the unit is `Chunk`, so a
  response that does not ask for delivery is byte-identical to one from a
  build without this stage. Changing the default requires an evaluation run
  (040) reported in the pull request, under the same rule as 018 B-10.

### 3.2 The budget

- **B-4 (what is counted).** `TokenBudget` is a hard cap on the
  model-facing payload: 019's framing statement and its version line, every
  envelope's opening and closing delimiters and labels, every delivered
  body text, and every cut and omission marker. Nothing the model reads in
  that payload is uncounted. The trace summary that 020 and 021 attach as
  metadata is not part of the payload; it is bounded by 018 B-5's summary
  form instead.
- **B-5 (the counter).** Tokens are counted by one named, versioned,
  in-process counter, whose identifier travels in the response. It makes
  no outbound call. A counter that cannot run falls back to a declared
  upper bound derived from the byte length that never under-counts, and the
  response names the fallback. The cap holds exactly under the named
  counter; against a client model with a different tokenizer the count is
  an estimate, and the response says which counter produced it.
- **B-6 (packing).** Packing walks the fused order once and is
  deterministic. Every reduction, for every rank, follows one ladder from
  the requested unit downwards: `Record`, then `Window { radius }` for
  each radius from the request's `window_radius` (default 1) down to 1,
  then `Chunk`. "Largest unit no larger than the requested one" means the
  first rung of that ladder, at or below the requested unit, that fits.
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
     dropped until it does not. The reported `used` is that recount.
- **B-7 (minimum budget).** A budget smaller than the framing statement,
  one envelope's delimiters and labels, a cut marker, and one token of body
  is refused with a typed error naming the minimum, rather than returning an
  empty or unframed response.
- **B-8 (server ceiling).** The deployment configures a default budget and
  a maximum. A request may lower the budget and may not exceed the maximum;
  a request above it is clamped and the response says so. The `Duration`
  budget of 018 B-9 is unchanged and independent.

### 3.3 Safety and determinism

- **B-9 (re-read under the caller's scope).** Neighbor chunks and whole
  bodies are read under the same scope and status predicates as 018 B-7,
  in the caller's scope only. A window never crosses into another memory,
  so a quarantined or erased memory can never reach the payload by being
  adjacent to a returned one.
- **B-10 (inside the envelope).** Every delivered text, whatever its unit,
  is emitted by 019's envelope function, with the memory id, trust class,
  origin, and the delivered byte ranges in the label. A cut marker is part
  of the envelope's contents and cannot be confused with a delimiter
  (019 B-7).
- **B-11 (reported per result).** Each delivered result reports the unit
  requested, the unit delivered, the reason (`fit`, `reduced`, `cut`,
  `conversation`), its byte ranges, and its token count. The response
  reports the budget, the tokens used, the counter, and the dropped ids.
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
  byte-identical payloads, and a request with no unit is byte-identical to
  the `Chunk` payload.
- **FR-007.** A test asserts the stage opens no socket and writes nothing.

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
whole record and which this spec does not carry. Grouping several memories
of one conversation into one delivered unit, which depends on how 031
stores a conversation (Q-1).

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

Open for the owner before approval:

- **Q-1.** Whether 031 stores an imported conversation as one memory or as
  one memory per message. If the latter, `Auto` needs a conversation unit
  that gathers sibling memories by their external reference, which this
  spec does not yet define.
- **Q-2.** Which counter is the default: a pinned tokenizer of a widely
  used model family, or the byte-derived upper bound alone.

## Verification

```verify:cli
cargo test -p aicortex-recall --locked --test delivery
cargo test -p aicortex-api --locked --test api
cargo test -p aicortex-mcp --locked --test tools
```
