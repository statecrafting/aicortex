# Lineage: what this corpus carried, and from where

Design notes are cited by specs and are never authoritative. Where this
document and a spec disagree, the spec wins and this document is stale.

## The clean-room rule

aicortex replaces a system called Open Brain (OB1), which is published under
FSL-1.1-MIT: a licence that forbids commercial derivative works until its
MIT conversion date. This repository is Apache-2.0 and is intended to be
sold. Therefore:

- No source, SQL, schema DDL, prompt text, or prose is copied from OB1 into
  this repository, in whole or in paraphrase.
- What is carried is *observation*: how the deployed system behaved, which
  failures it produced, and which capabilities its users valued. An
  observation is a fact about the world, not an expression, and it is cited
  as `openbrain://<topic>` so that every borrowing is visible and auditable.
- The import path in spec 042 reads OB1 *data* belonging to a user who asks
  for it. Reading a user's own rows is interoperability, and the shape of
  the request is derived from OB1's public HTTP surface as observed, not
  from its implementation.

If a session finds itself wanting to open an OB1 source file to answer a
design question, that is the signal to stop and specify from the problem
instead.

## Carried from the prior system as observations

| Citation | Observation | Where it lands |
|---|---|---|
| `openbrain://static-shared-key` | One static key in a query string authorized every user of every deployment, forever, with no rotation story | Constitution VII; spec 020 |
| `openbrain://service-role-everywhere` | Every function ran as the database service role, so row-level security was decorative | Constitution VII; spec 012 |
| `openbrain://json-bag-schema` | The metadata schema existed only inside a prompt string, so no two rows agreed | Constitution IX; spec 011 |
| `openbrain://orphaned-embedding` | Capture wrote the row and then the embedding as two calls; a failed second call left a memory that search could never reach | Constitution XI; spec 015 |
| `openbrain://unversioned-embeddings` | Rows carried no model or dimension, so a model change silently reinterpreted old vectors | Constitution XII; spec 015 |
| `openbrain://fork-per-feature` | Each capability was a fork of the whole server, so a fix in one never reached the others | Constitution VI; spec 030 |
| `openbrain://exact-dedup-only` | Deduplication was exact normalized text, so near-duplicates accumulated without bound | Spec 014; spec 034 |
| `openbrain://stats-loads-everything` | The statistics call read every row's metadata into memory | Spec 012 |
| `openbrain://no-schema-management` | The schema was SQL pasted from a guide, with no migrations and no versions | Spec 012 |
| `openbrain://valued-capabilities` | What users actually built on it: importers from every chat and read-later product, digests, consolidation, atomization, entity wikis, typed edges, provenance chains, per-agent identity and work claims | Specs 017, 030 to 035 |

## Carried from gbrain as observation

gbrain is a third party's MIT-licensed TypeScript memory system. Its licence
would permit more, but this corpus treats it exactly as it treats Open Brain:
no source, SQL, prompt text, or prose is copied, in whole or in paraphrase.
What is carried is observed behavior and measured results from its
published documentation, cited as `gbrain://<topic>`. Where a gbrain
behavior conflicts with the constitution, the observation is recorded and
the behavior is not carried; the specs that cite these rows say which.

| Citation | Observation | Where it lands |
|---|---|---|
| `gbrain://evidence-unit-auto` | Returning the whole conversation when a hit is part of a chat, and the ranked chunk otherwise, answered 445 of 500 LongMemEval-S development questions against 312 for chunks only and 457 for whole records, at about 15.1K against 3.5K reader tokens per question. An earlier run had neighbor windows well below whole sessions. A preregistered sealed set of 150 questions showed no benefit and no harm because its chats were short | Spec 025 |
| `gbrain://token-budget-cap` | An explicit evidence budget is a hard cap: the best-ranked hit is reserved first and cut with a marker rather than dropped, lower-ranked hits are kept as a rank prefix, and nothing spills past the cap. gbrain counts titles and evidence text but not the surrounding metadata | Spec 025 |
| `gbrain://retrieval-reflex` | Without a per-turn pointer, agents discuss an entity for several messages without retrieving its record. A compact pointer per resolvable entity in the user's message, built with no model call and doing nothing when it fails, closes the gap | Spec 026 |
| `gbrain://gap-note` | The answer feature users valued most was the gap note: nothing about a subject since a date, so the source may be stale | Spec 018 |
| `gbrain://external-benchmark` | Reporting on a public benchmark (LongMemEval) made results comparable outside the project | Spec 040 |
| `gbrain://sealed-confirmation-set` | A confirmation set whose design was fixed before results were seen caught a development-data gain that did not generalize, and showed that a set at its ceiling cannot discriminate | Spec 040 |
| `gbrain://eval-capture` | Capture of real queries for evaluation is off by default, scrubs personal data by default, records its own failures, and is kept apart from the judged fixtures | Specs 040, 041 |
| `gbrain://contradiction-probe` | An LLM judge over sampled retrieval results measures how often retrieved items contradict each other, distinguishes change over time from contradiction, reports a rate with a confidence interval as data, and never mutates the store | Spec 040 |
| `gbrain://embedding-migration-consent` | Re-embedding is previewed first (counts and estimated cost, no provider call) and then requires an operator-approved finite total cost cap; consent to run is not a substitute for the cap, and raising the cap authorizes a larger total rather than resetting what was spent | Spec 049 |
| `gbrain://retirement-valve` | A derived-content writer is held, writing nothing, when one run window would retire more than a capped number of items, and an empty result is held unless the source is proven empty | Spec 034 |
| `gbrain://memory-boundaries` | Durable knowledge (preferences, corrections, decisions, commitments, facts) belongs in shared memory; runtime state (task progress, tool output, local paths, enabled plugins, connection settings, credentials, activation state) does not, and a remembered preference installs or authorizes nothing | Specs 013, 022 |

## Carried from sibling repositories as design

- **rahi** supplies the chassis and is consumed as published crates. The
  contracts this product leans on hardest are `rahi://011` and `rahi://016`
  (the store, binary values, and the refusal of loadable extensions),
  `rahi://013` and `rahi://014` (the decision chain and its archive),
  `rahi://015` (the manifest ceiling and governed egress), `rahi://022` and
  `rahi://025` (the principal, and bearer tokens for non-browser clients),
  `rahi://026` (streaming), and `rahi://030` (the `Cell` trait and the
  verbs). Three of those, 016, 025, and 026, were written because this
  corpus needed them and the chassis was silent.
- **open-agentic-platform's session-memory package** is the direct ancestor
  of the memory vocabulary: memory kind, trust class, actor kind,
  importance and decay, quarantine, the write gate over carrier rules, and
  the harvesting discipline. It is the author's own prior work and the
  model is carried forward deliberately; the implementation is not, because
  it was TypeScript over `better-sqlite3` with no identity model.
- **butler-ai** contributes two patterns: the untrusted-content framing that
  becomes constitution X and spec 019, and the shape of a constraint spec
  that owns no code and freezes a privacy property across other specs'
  units, which becomes spec 041.
- **claude-observatory** contributes the decision-ledger discipline that
  informs spec 024, and is the first non-human producer of memories through
  spec 033.
- **hqgit** contributes the harness generation this repository was
  bootstrapped in: the session protocol, the per-spec `## Verification`
  block, and the constraint that a spec's acceptance criteria are commands
  with exit codes.

## What was deliberately not carried

- A second relational store, a hosted vector service, and an approximate
  index. Similarity is a paged scan in this process until the evaluation
  corpus says otherwise (constitution VIII, spec 016).
- An extension ecosystem in which a capability is a fork of the server. A
  capability is a source adapter, a curator, or a tool, registered against a
  contract (spec 030).
- A local stdio MCP server. The surface is remote streamable HTTP, which is
  what every target client speaks and what the chassis already secures.
