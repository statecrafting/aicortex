---
id: "016-vector-index-and-scan"
title: "The index: a paged cosine scan, an application-owned text index, and no SQLite extension"
status: approved
kind: "kernel"
domain: "retrieval"
created: "2026-09-03"
authors: ["Bartek Kus"]
implementation: pending
risk: high
wave: 1
depends_on:
  - "015-embedding-pipeline"
establishes:
  - "crates/aicortex-index/Cargo.toml"
  - "crates/aicortex-index/src/lib.rs"
  - "crates/aicortex-index/src/vector.rs"
  - "crates/aicortex-index/src/scan.rs"
  - "crates/aicortex-index/src/topk.rs"
  - "crates/aicortex-index/src/text.rs"
  - "crates/aicortex-index/src/tokenize.rs"
  - "crates/aicortex-index/src/migrations.rs"
  - "crates/aicortex-index/tests/scan.rs"
  - "crates/aicortex-index/tests/text.rs"
  - "crates/aicortex-index/benches/scan.rs"
extends:
  - { spec: "012-store-schema-and-repositories", unit: "crates/aicortex-store/src/migrations.rs", nature: additive }
summary: >
  Similarity is computed in this process. Vectors are BLOBs read through
  rahi's paged read, scored by dot product against a normalized query, and
  reduced by a bounded top-k heap, with the scope predicate in the SQL so a
  scan never leaves its scope. The text channel is an application-owned
  inverted index rather than FTS5, because FTS5 is not compiled into
  hiqlite's SQLite build and a loadable extension is refused by the chassis.
  A trait boundary exists so an approximate index can replace the scan when
  the evaluation corpus says it must, and not before.
---

# 016: The index

## 1. Purpose

The obvious way to do similarity search in SQLite is to load `sqlite-vec`.
The chassis refuses loadable extensions (`rahi://016`), because hiqlite
replicates statements and an engine that differs between nodes diverges
silently. The obvious way to do text search is FTS5, and hiqlite's rusqlite
is not built with the `fts5` feature, so it is not available either.

Both constraints point the same way: the index is application code over
ordinary tables and binary values. At personal and team scale this is not a
compromise. A scope of twenty thousand memories at 384 dimensions is about
thirty megabytes of vectors and scores in single-digit milliseconds, and the
implementation has no operational surface at all.

## 2. Territory

The `aicortex-index` crate and the `chunk_token` and `token_df` tables.
Extends 012's migration list. It owns no ranking policy: it returns
per-channel candidates with scores, and 018 decides the order.

## 3. Behavior

- **B-1 (vector layout).** `Vector` is a length-prefixed `f32`
  little-endian BLOB written by 015. Decoding validates the length against
  the recorded `dims` and returns a typed error on mismatch rather than
  reinterpreting bytes.
- **B-2 (the scan).** `scan(scope, revision, query, k, filter) ->
  Vec<Candidate>` reads `chunk_embedding` rows through rahi's
  `query_paged`, in pages of a configured size, scoring each page and
  retaining a bounded min-heap of size k. Peak memory is one page plus k,
  independent of scope size. The scope, the model revision, and the status
  filter are predicates in the SQL, never a post-pass.
- **B-3 (the metric).** Both sides are L2-normalized, so cosine similarity
  is a dot product. The kernel is written to be auto-vectorizable, with a
  scalar fallback, and a test asserts the two agree to within 1e-6 over a
  fixture.
- **B-4 (determinism).** Ties break by `(score desc, chunk_id asc)` so the
  same query and corpus always yield the same order. Floating point
  accumulation order is fixed by page and row order.
- **B-5 (the text channel).** `text.rs` maintains an inverted index:
  `chunk_token(chunk_id, token, tf)` and `token_df(token, df)`, updated in
  the same transaction as the chunk. Scoring is BM25 with the standard
  parameters, computed in process over the posting lists for the query's
  tokens, with the scope predicate in the SQL.
- **B-6 (tokenization).** Unicode-aware lowercase, word segmentation,
  optional light stemming for the configured language, a stop-word list,
  and a maximum token length. Tokenization is a pure function with a
  committed fixture set, because changing it changes every stored posting
  and is therefore a migration.
- **B-7 (the seam).** `trait VectorIndex { fn search(...) }` with
  `ScanIndex` as the only implementation. An approximate implementation may
  be added by a later spec only with an evaluation run (040) showing the
  recall it costs and the latency it buys, on a corpus at least ten times
  the size where the scan first exceeds its latency budget.
- **B-8 (budget).** The scan carries a soft deadline. Exceeding it returns
  the best candidates found so far, marked `truncated: true`, which 018
  records in the recall trace and 040 counts. Silent truncation does not
  exist.
- **B-9 (no extension, no custom function).** No `load_extension`, and no
  rusqlite scalar function registration: a function present on one node and
  not another is the same divergence hazard as an extension. A grep test
  asserts both.

## 4. Functional requirements

- **FR-001.** Over a 20000-chunk fixture, `scan` returns the same top-20 as
  a naive reference implementation, in the same order.
- **FR-002.** Peak allocation during a scan does not exceed one page plus
  k candidates, asserted with a counting allocator.
- **FR-003.** A scan for scope A returns no chunk of scope B, and a scan at
  revision 2 returns no chunk of revision 1.
- **FR-004.** BM25 scores match a reference implementation on a committed
  corpus to within 1e-6.
- **FR-005.** Tokenizer fixtures cover CJK text, accented Latin, hyphenated
  compounds, and code identifiers; a change to the tokenizer that alters
  any fixture fails the test.
- **FR-006.** A scan that exceeds its deadline returns `truncated: true`
  and a non-empty result.
- **FR-007.** The benchmark records scan latency at 1000, 20000, and
  200000 chunks, and the test asserts the 20000 case is under the budget
  on the CI machine.

## 5. Acceptance criteria

- **AC-1.** `cargo test -p aicortex-index --locked` passes.
- **AC-2.** `cargo bench -p aicortex-index` completes and writes the
  latency table that 040 consumes as a baseline.

## 6. Out of scope

Fusion and ranking policy (018). Graph proximity (017). Approximate
indexes, which are gated on evidence by B-7. Cross-encoder reranking,
which would require a second model and is deferred until evaluation shows
the need.

## 7. Resolved decisions

- **D-1 (2026-09-03, this spec).** The text channel is an
  application-owned inverted index rather than FTS5. Verified: hiqlite
  builds rusqlite with `backup`, `bundled`, `cache`, `chrono`,
  `modern_sqlite`, `column_decltype`, `csvtab`, `functions`,
  `load_extension`, `serde_json`, `series`, `url`, `uuid`, and `vtab`, and
  no `fts5`. Depending on FTS5 would mean depending on a hiqlite build
  change, which is a dependency this product cannot control. Rejected
  alternative: `LIKE` matching, which cannot rank.
- **D-2 (2026-09-03, this spec).** Brute-force scan first, with a trait
  seam. An approximate index adds an index-build lifecycle, a staleness
  question, and a recall cost, none of which are justified before the
  evaluation corpus exists and shows the scan failing its budget.

## Verification

```verify:cli
cargo test -p aicortex-index --locked
```
