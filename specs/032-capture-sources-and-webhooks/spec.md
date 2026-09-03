---
id: "032-capture-sources-and-webhooks"
title: "Live capture: signed webhooks from chat systems and the browser, quarantined by default"
status: approved
kind: "feature"
domain: "ingest"
created: "2026-09-03"
authors: ["Bartek Kus"]
implementation: pending
risk: high
wave: 3
depends_on:
  - "031-conversation-and-archive-imports"
establishes:
  - "crates/aicortex-ingest/src/webhook/mod.rs"
  - "crates/aicortex-ingest/src/webhook/verify.rs"
  - "crates/aicortex-ingest/src/webhook/chat.rs"
  - "crates/aicortex-ingest/src/webhook/browser.rs"
  - "crates/aicortex-ingest/src/webhook/binding.rs"
  - "crates/aicortex-ingest/tests/webhook.rs"
extends:
  - { spec: "030-source-adapter-framework", unit: "crates/aicortex-ingest/src/registry.rs", nature: additive }
  - { spec: "020-http-api-and-scopes", unit: "crates/aicortex-api/src/router.rs", nature: additive }
references:
  - { unit: { kind: file, path: "specs/019-untrusted-content-boundary/spec.md" }, role: constraint }
summary: >
  Capture that happens while you work: a message forwarded from a chat
  system, a page clipped from the browser. Both arrive as webhooks, which
  means both are unauthenticated in the OAuth sense and must be treated
  accordingly. Signatures are verified per platform, the external identity
  is bound to a local subject by an explicit linking step, and content from
  a channel other people can write to is quarantined by default rather than
  trusted because it arrived over a verified transport.
---

# 032: Live capture

## 1. Purpose

The predecessor had capture integrations for the major chat systems and a
browser extension, and they were among the most used pieces
(`openbrain://valued-capabilities`). They were also the least defensible: a
webhook that anyone could reach with the shared key wrote directly into the
store.

The distinction this spec insists on is between transport authenticity and
content trust. A verified platform signature proves the message came from
that platform. It proves nothing about whether the person who typed it is
the owner of this memory scope, or whether the text is an attempt to write
an instruction into someone's assistant.

## 2. Territory

The webhook modules inside `aicortex-ingest` and their routes.

## 3. Behavior

- **B-1 (signature verification).** Each platform's documented signature
  scheme is verified with a constant-time comparison and a timestamp window
  against replay. A request that fails verification is dropped with 401 and
  counted; it never reaches the gate.
- **B-2 (identity binding).** An external identity is bound to a local
  subject by a linking flow the subject completes while authenticated: the
  platform user id is stored against the scope. An unbound external
  identity produces no memory, and its attempts are counted and visible so
  the user can see that something is trying.
- **B-3 (explicit capture, not ambient).** Capture is triggered by an
  explicit act in the source platform: a forward to the app, a reaction, a
  slash command, an extension click. There is no mode that ingests a
  channel wholesale, because that would make the memory store a copy of a
  workspace with none of its access controls.
- **B-4 (trust).** A message authored by the bound subject is an
  `Assertion` by a `Human` actor. A message authored by anyone else, or a
  clipped page, is `Public` in the sense of 030 B-7 and is quarantined for
  review (023).
- **B-5 (browser clips).** The extension sends the selection, the page URL,
  the title, and the capture time. It never sends the whole DOM, cookies,
  or any credential visible on the page, and the payload schema is closed
  so it cannot.
- **B-6 (no bodies in acknowledgements).** A webhook response is an
  acknowledgement with an id and a state, never an echo of content, so a
  platform's message log does not become a second copy.
- **B-7 (rate and size).** Per-binding rate limits and payload ceilings,
  enforced before parsing.
- **B-8 (the extension is a client, not a special case).** The browser
  extension authenticates as an OAuth client (022 B-5) when the browser can
  hold a session, and falls back to the signed-webhook path only where it
  cannot.

## 4. Functional requirements

- **FR-001.** A payload with a tampered signature is rejected with 401 and
  writes nothing; a replayed payload outside the window likewise.
- **FR-002.** A payload from an unbound external identity writes no memory
  and increments a visible counter.
- **FR-003.** A message authored by someone other than the bound subject
  lands quarantined.
- **FR-004.** A browser clip payload containing extra fields is rejected
  by the closed schema.
- **FR-005.** The acknowledgement body contains no substring of the
  captured content.

## 5. Acceptance criteria

- **AC-1.** `cargo test -p aicortex-ingest --locked --test webhook` passes.
- **AC-2.** At least one chat platform is verified end to end against a
  running instance, recorded in the spec's Status note.

## 6. Out of scope

Publishing an extension to a browser store, which is a release concern.
Outbound posting into chat platforms, which this product does not do.

## 7. Resolved decisions

- **D-1 (2026-09-03, this spec).** Quarantine by default for anything not
  authored by the bound subject. The alternative, trusting a verified
  transport, is exactly how a memory store becomes an injection vector for
  anyone who can post in a shared channel.

## Verification

```verify:cli
cargo test -p aicortex-ingest --locked --test webhook
```
