---
id: "002-memory-thesis"
title: "Memory thesis: nine responsibilities, one governed cell, provider agnostic by protocol"
status: approved
kind: "thesis"
domain: "governance"
created: "2026-09-03"
authors: ["Bartek Kus"]
implementation: n-a
risk: critical
wave: 1
depends_on:
  - "000-aicortex-bootstrap"
constrains:
  - kind: sequencing-plan
    target_specs:
      - "010-chassis-adoption-and-workspace"
      - "011-memory-model"
      - "012-store-schema-and-repositories"
      - "013-write-gate-and-redaction"
      - "014-memory-lifecycle-and-erasure"
      - "015-embedding-pipeline"
      - "016-vector-index-and-scan"
      - "017-entities-and-typed-edges"
      - "018-retrieval-and-recall-trace"
      - "019-untrusted-content-boundary"
      - "020-http-api-and-scopes"
      - "021-mcp-server"
      - "022-client-adapters"
      - "023-review-and-promotion"
      - "024-decision-and-audit-references"
      - "030-source-adapter-framework"
      - "031-conversation-and-archive-imports"
      - "032-capture-sources-and-webhooks"
      - "033-observatory-and-agent-ingestion"
      - "034-curation-workers"
      - "035-agent-coordination"
      - "040-retrieval-evaluation"
      - "041-privacy-and-egress-boundary"
      - "042-portability-and-migration"
      - "043-packaging-and-operations"
      - "044-reference-deployment"
references:
  - { unit: { kind: file, path: "docs/design/00-lineage.md" }, role: context }
summary: >
  What aicortex is, what it refuses, and the order it is built in. It is
  persistent memory for AI clients, delivered as a governed cell on the rahi
  chassis, reached by any client that speaks MCP over streamable HTTP. Nine
  responsibilities across eight domains, thirteen crates and one binary,
  four build waves. The refusals are as load-bearing as the claims: no
  second store, no static key, no fork-per-feature, no local stdio server,
  and no memory that a model may treat as an instruction unless a human
  promoted it.
---

# 002: Memory thesis

## 1. What it is

An AI client forgets everything between sessions. The workarounds are a
file the user maintains by hand, a vendor's closed memory that does not
leave that vendor, or a vector database with no notion of who said a thing,
when, or whether it is still true. aicortex is the third option done
properly: one store the user owns, one protocol every client already
speaks, and enough structure that recall is explainable and stale facts can
be corrected rather than accumulating.

The product is defensible because of the parts that are boring: identity
that is real, capture that cannot lose a memory, embeddings that name their
model, provenance on every row, forgetting that actually forgets, and an
evaluation corpus that says whether a change to ranking helped. The system
this replaces had none of those and was pleasant to demonstrate.

## 2. The nine responsibilities

1. **The cell.** A binary that implements rahi's `Cell`: a manifest,
   migrations, routes, operator routes, one line of `main`. Identity,
   state, the ledger, the kernel, the edge, packaging, and the verbs are
   the chassis's and are consumed by version (domain `chassis`).
2. **The record.** What a memory is: scope, content, actor, source, time,
   trust class, schema version, and the provenance that makes it a claim by
   someone rather than a floating string (domain `memory`).
3. **Admission.** The write gate that runs before the transaction: secret
   refusal, size ceiling, carrier rules, quarantine for content whose
   origin cannot be established (domain `memory`).
4. **Lifecycle.** Supersession, correction, expiry, decay, and erasure that
   reaches content, chunks, embeddings, and index entries (domain
   `memory`).
5. **Understanding.** Embeddings produced by a declared provider through
   the outbox, entities and typed edges extracted with their extractor
   version, both derived and both subordinate to the memory they came from
   (domains `memory`, `retrieval`).
6. **Recall.** Hybrid retrieval over vectors, text, recency, and graph
   proximity, fused into one order that can be explained, with every result
   labelled and delimited as untrusted input (domain `retrieval`).
7. **Surfaces.** An MCP server over streamable HTTP and an HTTP API, both
   authorized by rauthy-issued bearer tokens, plus the per-client adapters
   that make capture automatic (domains `protocol`, `clients`).
8. **Intake.** A source adapter contract, the importers that bring a user's
   history in, the webhooks that capture from chat systems, and the
   orchestrator's push of session decisions (domain `ingest`).
9. **Proof.** The evaluation corpus, the privacy and egress boundary, the
   portable export, packaging, and the reference deployment (domain `ops`).

## 3. Provider agnostic, by protocol not by adapter

Claude Code, Codex, Cursor, and Antigravity all speak MCP over streamable
HTTP and all support OAuth 2.1 against a remote server. One server
therefore serves all four, and the core has no client-specific code. What
differs between them is only how capture is made automatic: Claude Code
hooks, Codex notifications, Cursor hooks, Antigravity's configuration.
That difference is quarantined in one spec (022) that owns a capability
matrix and the install documentation, so a fifth client is a row in a table
and not a change to the server.

## 4. Crate topology

| Layer | Crates | Founding specs |
|---|---|---|
| types | `aicortex-types` | 011 |
| admission | `aicortex-store`, `aicortex-gate` | 012, 013, 014 |
| understanding | `aicortex-embed`, `aicortex-index`, `aicortex-graph` | 015, 016, 017 |
| recall | `aicortex-recall` | 018 |
| surfaces | `aicortex-api`, `aicortex-mcp` | 020, 021 |
| intake | `aicortex-ingest` | 030 |
| curation | `aicortex-curate` | 034 |
| proof | `aicortex-eval` | 040 |
| the cell | `apps/aicortex` | 010 |

Dependencies point downward only. Every crate depends on the rahi crates
rather than reimplementing them, and `apps/aicortex` composes the lot.
Nothing depends on the app.

## 5. Build order

Wave 1 (010 to 019) is the memory core and is buildable and testable with
no network listener: a temp directory, a single-voter hiqlite through
rahi's harness, and fixtures. Wave 2 (020 to 029) adds the surfaces and
needs a rauthy binary on loopback. Wave 3 (030 to 039) adds intake and
curation. Wave 4 (040 to 049) adds proof: evaluation, the privacy
boundary, portability, packaging, and the reference deployment. Within a
wave the ordinal is the order; across waves every `depends_on` points into
the same or an earlier wave.

The wave 2 exit condition is a real MCP client completing OAuth against a
running instance and capturing a memory. The wave 4 exit condition is the
reference deployment on Kubernetes with the evaluation corpus green.

## 6. What the thesis refuses

- **A static shared key.** No credential originates in this codebase. The
  system this replaces shipped one key, in a query string, for every user
  of every deployment, forever (`openbrain://static-shared-key`).
- **A second store.** No external vector database, no separate relational
  store, no loadable SQLite extension. Similarity is a paged scan over
  binary values in this process until the evaluation corpus proves it is
  not enough (`rahi://016`).
- **Fork per feature.** A capability is a source adapter, a curator, or a
  tool registered against a contract. It is never a copy of the server
  (`openbrain://fork-per-feature`).
- **A local stdio server.** The surface is remote streamable HTTP. A local
  process per client multiplies deployments and cannot be governed.
- **A memory the model may obey.** Everything returned is data. Promotion
  to instruction grade is a human decision recorded in the ledger, and an
  agent never promotes its own memory.
- **A schema that lives in a prompt.** Structure is columns and migrations.
  A model may propose metadata; it never defines the shape
  (`openbrain://json-bag-schema`).
- **Silent quality regression.** A change to chunking, embedding, ranking,
  or fusion without an evaluation run is not shippable.

## 7. Resolved decisions

- **D-1 (2026-09-03, thesis).** aicortex is a governed cell on rahi rather
  than a standalone binary with its own HTTP and auth. The cost is a
  dependency on a chassis that is itself unbuilt; the benefit is that
  identity, packaging, the ledger, and the kernel arrive correct and are
  shared with the other product in the family. Rejected alternative: a
  self-contained axum binary that reimplements the same seven
  responsibilities and drifts from them.
- **D-2 (2026-09-03, thesis).** One deployment serves both the personal
  single-user case and the plane case, distinguished by configuration and
  scope, not by two codebases. A second build target would double every
  surface.
- **D-3 (2026-09-03, thesis).** Local embedding is the default and remote
  providers are opt-in. A memory system whose pitch is that the user owns
  the data cannot send every captured sentence to a third party by
  default, and the privacy boundary (041) must be provable from the
  manifest rather than promised.

## Verification

```verify:cli
scripts/spec-dag.sh
```
