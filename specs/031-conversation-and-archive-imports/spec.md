---
id: "031-conversation-and-archive-imports"
title: "Bringing a history in: chat exports, note vaults, and read-later archives"
status: approved
kind: "feature"
domain: "ingest"
created: "2026-09-03"
authors: ["Bartek Kus"]
implementation: pending
risk: medium
wave: 3
depends_on:
  - "011-memory-model"
  - "012-store-schema-and-repositories"
  - "030-source-adapter-framework"
establishes:
  - "crates/aicortex-ingest/src/sources/mod.rs"
  - "crates/aicortex-ingest/src/sources/chat_export.rs"
  - "crates/aicortex-ingest/src/sources/vault.rs"
  - "crates/aicortex-ingest/src/sources/readlater.rs"
  - "crates/aicortex-ingest/src/sources/mail.rs"
  - "crates/aicortex-ingest/src/upload.rs"
  - "crates/aicortex-ingest/tests/sources.rs"
  - "crates/aicortex-ingest/testdata/exports/"
extends:
  - { spec: "030-source-adapter-framework", unit: "crates/aicortex-ingest/src/registry.rs", nature: additive }
  - { spec: "011-memory-model", unit: "crates/aicortex-types/src/provenance.rs", nature: additive }
  - { spec: "012-store-schema-and-repositories", unit: "crates/aicortex-store/src/migrations.rs", nature: additive }
  - { spec: "012-store-schema-and-repositories", unit: "crates/aicortex-store/src/provenance_repo.rs", nature: additive }
  - { spec: "020-http-api-and-scopes", unit: "crates/aicortex-api/src/router.rs", nature: additive }
references:
  - { unit: { kind: file, path: "specs/019-untrusted-content-boundary/spec.md" }, role: constraint }
  - { unit: { kind: file, path: "specs/025-evidence-delivery-and-token-budget/spec.md" }, role: context }
summary: >
  A memory system is worth little on day one and a great deal once it knows
  what you already said. This spec brings a user's existing history in: chat
  exports from the major assistants, a note vault on disk, a read-later
  archive, and mail, each as an adapter over the framework. A conversation
  is stored as one memory per message, linked by a shared conversation id
  and a turn number in provenance, so each turn keeps its own author and
  trust class and can be redacted or forgotten alone. The interesting work
  is not fetching, it is extracting from those turns the memories worth
  keeping, and doing it without sending the archive anywhere.
---

# 031: Imports

## 1. Purpose

Users of the predecessor built importers for every chat product, note
vault, and read-later service they used, which is the clearest signal in
its ecosystem about what people actually want from a memory store
(`openbrain://valued-capabilities`). Each was a fork; here they are five
modules over one runner.

## 2. Territory

Five adapter modules and the upload path, inside `aicortex-ingest`.

B-9 to B-11 (amended 2026-10-30, D-2) reach units other specs own only
through `extends` edges: 011's `provenance.rs` gains the optional
conversation reference, and 012's migrations and provenance repository
gain its nullable columns, its index, and the scope-predicated read of a
conversation's turns. Spec 025, if it is built first, is to add the same field
exactly as B-9 defines it; 025 is a draft, and its section 2 takes on
that obligation through its own amendment, which is reviewed separately.
Until then this sentence is a requirement on 025, not a description of
it, and FR-010 is what enforces it. This spec's build then uses it
rather than adding it again. The check is concrete: if `Provenance` in
`crates/aicortex-types/src/provenance.rs` already has a `conversation`
field and the store already has `conversation_turns`, this build adds no
field, column, index, or read, and FR-010 and FR-011 must pass against
the existing ones; otherwise this build adds them. 025 is deliberately not a `depends_on`
entry: it is a draft, and a dependency would hold this approved spec back
until 025 is approved and built, although nothing here needs 025 to
exist. It is listed under `references`, and FR-010's fixture is the
mechanical check that the two definitions agree in whichever order they
are built.

## 3. Behavior

- **B-1 (uploads are files, not URLs).** An export is uploaded to a
  scope-owned staging area, processed, and deleted on completion or
  expiry. The archive is never fetched from a third party on the user's
  behalf, so no credential for another service is stored.
- **B-2 (chat exports).** One adapter with per-product parsers for the
  common assistant export formats, each detected by content rather than by
  filename. A conversation becomes one memory per message (B-9), plus
  candidate memories for the durable content inside it (B-3). There is no
  single memory holding the whole thread (amended 2026-10-30, D-2).
- **B-3 (what a transcript yields).** The default extraction keeps
  decisions, corrections, stated preferences, and facts about people and
  projects, and discards pleasantries, restatements, and code blocks
  already in a repository. The rule set is data with a committed fixture
  corpus, and it is deterministic: a model is not required to import.
  Extraction reads one message at a time: each candidate comes from exactly
  one message, names that message's memory in `derived_from` (011 B-8), and
  so has exactly one author. A candidate does not carry the conversation
  reference itself; it reaches its conversation through the message it was
  derived from.
- **B-4 (attribution).** Every memory from a transcript records who said
  it: the user, the assistant, or a named participant. An assistant's claim
  is `Assertion` by an `Agent` actor and never becomes `Evidence` merely by
  being imported.
- **B-5 (note vault).** A directory of markdown with front matter and
  wiki-style links. Files map to memories, front matter maps to typed
  fields, links become graph edges (017), and a re-run reconciles: changed
  files supersede, deleted files expire.
- **B-6 (read-later and mail).** Read-later items import as `Reference`
  memories with highlights as children. Mail imports through the user's own
  authenticated account with a query the user supplies, never a full
  mailbox sweep by default, and it is a `Public` source in the sense of 030
  B-7 for anything not sent by the user.
- **B-7 (progress and dry run).** Imports are the primary consumer of dry
  runs (030 B-10) and of the event stream: a ten-year archive reports
  counts per category before the user commits.
- **B-8 (locality).** Parsing, extraction, and embedding of an import all
  happen in process. An import does not enable any egress the deployment
  did not already have.

### Conversations as messages (amended 2026-10-30, D-2)

- **B-9 (one memory per message).** Every message of an imported
  conversation that has text is stored as its own memory. A message with
  no text (an attachment or tool call alone) is skipped and counted in the
  run report. `Provenance` gains `conversation: Option<ConversationRef>`,
  serialized only when present, so every existing 011 fixture round-trips
  byte-identically (011 FR-001). `ConversationRef { id: ConversationId,
  turn: u32 }`, whose wire form is the provenance key `conversation`
  holding an object with exactly the keys `id` (a string) and `turn` (an
  unsigned integer): `id` is the export's own conversation identifier, or, when
  the export has none, a derived id: the string `d:` followed by the
  lowercase hexadecimal SHA-256 of the UTF-8 bytes of the parser's
  identifier, the conversation's creation time as the export states it
  (or, if it states none, the timestamp of the first message that has
  text) in RFC 3339 UTC, and the text of the first message that has text,
  joined by a single NUL byte. The join is unambiguous: a parser
  identifier and an RFC 3339 time contain no NUL by construction, and the
  text, which may, is the last field. A conversation with no message that
  has text stores nothing and needs no id. Those inputs
  are fixed by the export, so the id is the same on every re-import of
  the same export and on a later export that still holds the
  conversation. Two conversations of one export that derive the same id
  are not merged: the second is refused, counted in the run report, and
  listed in the dry run; `turn`
  is the message's 1-based position in the export's message order,
  assigned before skipping, so a skipped message leaves a gap rather than
  renumbering the turns after it. Two message memories are turns of one
  conversation exactly when they share a scope, a source system (011 D-3),
  and a conversation id; the same id in another scope or from another
  source names a different conversation. Each message's `SourceRef`
  external id is the export's message id, or `<conversation id>/<turn>`
  when the export has none, so 030 B-4 keeps a re-import idempotent per
  message.
- **B-10 (each turn its own author).** Actor and trust class are set per
  message under B-4: an assistant turn is `Assertion` by an `Agent` actor,
  and a user turn is attributed to the user with the verdict 030 B-7 and
  the gate (013 B-7) give an export. No memory ever mixes the turns of two
  authors, so a trust class is never an average over a conversation. The
  write gate, redaction, supersession, and erasure (013, 014) act on one
  message at a time: redacting or forgetting one turn leaves every other
  turn of the conversation untouched. A message longer than 013 B-6's
  ceiling is refused by the gate like any other candidate, with its reason
  code counted in the run report and listed by turn in the dry run.
- **B-11 (rebuilding a conversation).** The store indexes the conversation
  reference by scope, source system, conversation id, and turn, and
  exposes one read, `conversation_turns(scope, source, id, turns)`, that
  returns a conversation's message memories in turn order under the same
  scope and status predicates as every other read (012, 018 B-7), so a
  quarantined, expired, or erased turn is absent from it. `turns` is a
  `TurnRange`: either `All`, or `Between { first, last }`, an inclusive
  range of turn numbers with `first <= last` (a reversed range is a typed
  error, not an empty answer). It selects by turn number, not by count, so
  a range over a gap returns fewer memories than its width; it never
  substitutes a turn from outside the range to fill one. This is the seam
  wider reads use: 025's `Window` returns the turns around a hit and its
  `Record` rebuilds the whole conversation from the shared id. This spec
  adds no route and no tool for it.

## 4. Functional requirements

- **FR-001.** Each committed export fixture imports to the recorded memory
  count, kinds, and attributions.
- **FR-002.** A re-import of the same export produces zero new memories.
- **FR-003.** A vault re-run after editing one file supersedes exactly one
  memory; after deleting one file, expires exactly one.
- **FR-004.** With the local provider configured, an import opens no
  socket.
- **FR-005.** A transcript containing the 019 injection fixture stores it
  as content and returns it only inside an envelope.
- **FR-006.** A committed export fixture of one conversation with user and
  assistant turns, one message with no text, and one message over the body
  ceiling imports to one memory per stored message, each carrying the
  conversation id and its turn, with the skipped and refused turns counted
  and the turn numbers of the others unchanged by the gaps. Every assistant
  turn is `Assertion` by an `Agent` actor, and no stored memory has more
  than one author.
- **FR-007.** Erasing one message memory of an imported conversation
  leaves every other turn's body, chunks, and embeddings intact, and
  `conversation_turns` then returns the remaining turns in order without
  the erased one. The same holds for redacting or superseding one turn.
- **FR-008.** Two fixture conversations with the same conversation id, one
  in another scope and one from another source system, never appear in
  each other's `conversation_turns`, and a re-import of the fixture
  creates no new message memory (FR-002).
- **FR-009.** Every memory fixture of 011 round-trips byte-identically
  after `conversation` is added to `Provenance`: 011's own round-trip test
  (011 FR-001) passes unchanged.
- **FR-010.** A committed fixture pins `ConversationRef`'s wire form
  (B-9): a provenance carrying a conversation reference serializes to that
  fixture byte for byte and deserializes from it, and a provenance with an
  extra, missing, or renamed key under `conversation` is refused with a
  typed error. Whichever of this spec and 025 is built second runs this
  test against the field the first one added, so a divergent definition
  fails the build rather than passing silently.
- **FR-011.** `conversation_turns` with `All` returns every `Active` turn
  in order; with `Between` over a range containing an erased turn returns
  the others in the range and nothing outside it; and with a reversed
  range returns the typed error.
- **FR-012.** For a committed export fixture whose conversations carry no
  identifier, two imports derive the same `d:` ids, each equal to the
  value recorded in the fixture, and two conversations in one fixture that
  derive the same id import as one conversation plus one refusal counted
  in the run report.

## 5. Acceptance criteria

- **AC-1.** `cargo test -p aicortex-ingest --locked --test sources` passes.
- **AC-2.** A real export of at least 1000 conversations imports end to
  end, with the counts recorded in the spec's Status note.
- **AC-3.** `cargo test -p aicortex-types --locked` and `cargo test -p
  aicortex-store --locked` pass with the conversation reference added
  (FR-009, B-11).

## 6. Out of scope

Live capture from chat systems (032). Format-specific parsers for products
with no export, which are added as new parsers behind B-2's detection.

## 7. Resolved decisions

- **D-1 (2026-09-03, this spec).** Extraction from transcripts is
  deterministic by default rather than model-driven. An import of ten
  thousand conversations through a model is slow, expensive, non-repeatable,
  and, with a remote provider, a bulk disclosure of the user's entire
  history. A model-assisted pass is available per conversation, on request.
- **D-2 (2026-10-30, owner decision, owner-directed amendment).** Recorded
  from the owner's answer to 025 Q-1 on 2026-10-30, which assigned the
  decision to this spec: "store one memory per message. Link the messages
  with a shared conversation id and a turn number in their provenance."
  The owner's reasons, which this amendment carries: each turn has its own
  author, and a user turn and an assistant turn carry different trust
  classes, which one memory per conversation would blur; the write gate,
  redaction, erasure, and supersession all work on one memory at a time,
  so per-message storage lets one turn be redacted or forgotten without
  touching the whole chat; and wider reads stay available, because 025's
  `Window` can return the turns around a hit and its `Record` can rebuild
  the conversation from the shared id. B-2 is amended, B-3 is narrowed to
  one message per candidate, and B-9 to B-11, FR-006 to FR-012, and AC-3
  are added, with the matching Verification lines. Rejected: one `Reference` memory per thread, which was B-2's
  earlier text. Because the owner directed this amendment, this spec stays
  `status: approved`: constitution V makes approval a human act, and the
  human authority who approves this corpus decided the content of this
  amendment, so it is not a machine-authored change awaiting review.

## Verification

```verify:cli
cargo test -p aicortex-ingest --locked --test sources
cargo test -p aicortex-types --locked
cargo test -p aicortex-store --locked
```
