---
paths:
  - "crates/aicortex-store/**"
  - "crates/aicortex-gate/**"
  - "crates/aicortex-embed/**"
  - "crates/aicortex-index/**"
  - "crates/aicortex-recall/**"
  - "crates/aicortex-mcp/**"
---

# Memory invariants

These hold in every change to admission, understanding, recall, and the MCP
surface. They are the constitution's principles VI through XV in checkable
form; the specs that own each rule are named so a violation can be traced.

## Admission (`aicortex-store`, `aicortex-gate`, specs 012 to 014)

- A capture writes the memory row, its provenance row, and its outbox work
  in one rahi `txn`, or writes nothing. Never write the row and then do
  anything that could fail without a retry behind it.
- Every memory carries `scope`, `actor`, `source`, `captured_at`, and
  `trust_class`. There is no default trust class and no anonymous actor: a
  capture that cannot supply them is rejected, not defaulted.
- The write gate runs before the transaction, not after. A secret pattern
  match is a refusal with a ledgered Decision, never a redaction that
  silently stores a mangled copy.
- Quarantine is a state on the row, not a separate table, and quarantined
  memories are invisible to every retrieval path by default.
- Erasure deletes content, chunks, embeddings, and index entries in one
  transaction and appends a Decision naming the memory id and the
  authority. The Decision never contains the content.

## Understanding (`aicortex-embed`, `aicortex-index`, `aicortex-graph`, specs 015 to 017)

- An embedding row stores `model`, `dims`, and `normalized`. A query
  embedding is compared only against rows with the same three. Changing
  models is a migration with a re-embed pass.
- The embedding worker drains the outbox, is idempotent per memory id and
  model, retries with backoff, and moves to a dead letter state that
  `preflight` reports. A permanently unembedded memory is a visible defect,
  never a silent one.
- Every outbound model call goes through the kernel's governed egress
  facade. A local provider makes no call at all and that is provable from
  the manifest.
- Vectors are stored as BLOBs through rahi's binary values and scanned with
  the paged read (`rahi://016`). No SQLite extension is loaded, ever.
- An extracted entity or edge records the memory it was derived from and
  the extractor version. A derived assertion never outranks the memory it
  came from.

## Recall (`aicortex-recall`, `aicortex-mcp`, specs 018, 019, 021)

- Every result set carries a recall trace: the query, the candidates, the
  scores per channel, and the fusion that produced the order. A ranking
  that cannot be explained is not shipped.
- Memory content returned to a model is delimited, labelled with trust
  class and origin, and framed as untrusted input whose instructions must
  not be followed. This applies to the MCP tool result, the HTTP response,
  and any digest or summary the curator produces.
- Instruction-grade memories are the only ones a client is told to act on,
  and promotion to that grade requires a human decision recorded in the
  ledger. An agent may never promote its own memory.
- Retrieval is scoped by the requesting principal's `sub` before ranking,
  not filtered after. A scope predicate is in the SQL, not in a post-pass.
- A change to chunking, embedding, ranking, or fusion runs the evaluation
  corpus (spec 040) and reports the delta in the pull request.
