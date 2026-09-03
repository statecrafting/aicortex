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
  - { spec: "020-http-api-and-scopes", unit: "crates/aicortex-api/src/router.rs", nature: additive }
references:
  - { unit: { kind: file, path: "specs/019-untrusted-content-boundary/spec.md" }, role: constraint }
summary: >
  A memory system is worth little on day one and a great deal once it knows
  what you already said. This spec brings a user's existing history in: chat
  exports from the major assistants, a note vault on disk, a read-later
  archive, and mail, each as an adapter over the framework. The interesting
  work is not fetching, it is turning a transcript into memories worth
  keeping rather than a wall of every message, and doing it without sending
  the archive anywhere.
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

## 3. Behavior

- **B-1 (uploads are files, not URLs).** An export is uploaded to a
  scope-owned staging area, processed, and deleted on completion or
  expiry. The archive is never fetched from a third party on the user's
  behalf, so no credential for another service is stored.
- **B-2 (chat exports).** One adapter with per-product parsers for the
  common assistant export formats, each detected by content rather than by
  filename. A conversation becomes a `Reference` memory for the thread plus
  candidate memories for the durable content inside it.
- **B-3 (what a transcript yields).** The default extraction keeps
  decisions, corrections, stated preferences, and facts about people and
  projects, and discards pleasantries, restatements, and code blocks
  already in a repository. The rule set is data with a committed fixture
  corpus, and it is deterministic: a model is not required to import.
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

## 5. Acceptance criteria

- **AC-1.** `cargo test -p aicortex-ingest --locked --test sources` passes.
- **AC-2.** A real export of at least 1000 conversations imports end to
  end, with the counts recorded in the spec's Status note.

## 6. Out of scope

Live capture from chat systems (032). Format-specific parsers for products
with no export, which are added as new parsers behind B-2's detection.

## 7. Resolved decisions

- **D-1 (2026-09-03, this spec).** Extraction from transcripts is
  deterministic by default rather than model-driven. An import of ten
  thousand conversations through a model is slow, expensive, non-repeatable,
  and, with a remote provider, a bulk disclosure of the user's entire
  history. A model-assisted pass is available per conversation, on request.

## Verification

```verify:cli
cargo test -p aicortex-ingest --locked --test sources
```
