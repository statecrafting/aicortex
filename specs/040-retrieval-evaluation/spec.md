---
id: "040-retrieval-evaluation"
title: "Evaluation: a golden corpus, recall and precision at k, and a gate that refuses a silent regression"
status: draft
kind: "kernel"
domain: "ops"
created: "2026-09-03"
authors: ["Bartek Kus"]
implementation: pending
risk: critical
wave: 4
depends_on:
  - "018-retrieval-and-recall-trace"
  - "035-agent-coordination"
establishes:
  - "crates/aicortex-eval/Cargo.toml"
  - "crates/aicortex-eval/src/lib.rs"
  - "crates/aicortex-eval/src/corpus.rs"
  - "crates/aicortex-eval/src/metrics.rs"
  - "crates/aicortex-eval/src/runner.rs"
  - "crates/aicortex-eval/src/report.rs"
  - "crates/aicortex-eval/src/main.rs"
  - "crates/aicortex-eval/tests/metrics.rs"
  - "eval/README.md"
  - "eval/golden/"
  - "eval/baselines/"
  - "crates/aicortex-eval/src/external.rs"
  - "crates/aicortex-eval/src/prereg.rs"
  - "crates/aicortex-eval/src/judge.rs"
  - "crates/aicortex-eval/src/contradictions.rs"
  - "crates/aicortex-eval/tests/external.rs"
  - "crates/aicortex-eval/tests/prereg.rs"
  - "crates/aicortex-eval/tests/contradictions.rs"
  - "crates/aicortex-store/src/query_capture.rs"
  - "crates/aicortex-store/tests/query_capture.rs"
  - "eval/external/"
  - "eval/prereg/"
extends:
  - { spec: "018-retrieval-and-recall-trace", unit: "crates/aicortex-recall/src/lib.rs", nature: additive }
  - { spec: "012-store-schema-and-repositories", unit: "crates/aicortex-store/src/lib.rs", nature: additive }
  - { spec: "012-store-schema-and-repositories", unit: "crates/aicortex-store/src/migrations.rs", nature: additive }
  - { spec: "014-memory-lifecycle-and-erasure", unit: "crates/aicortex-store/src/erasure.rs", nature: additive }
  - { spec: "020-http-api-and-scopes", unit: "crates/aicortex-api/src/search.rs", nature: additive }
references:
  - { unit: { kind: file, path: "docs/design/00-lineage.md" }, role: context }
  - { unit: { kind: file, path: "specs/041-privacy-and-egress-boundary/spec.md" }, role: constraint }
  - { unit: { kind: file, path: "specs/020-http-api-and-scopes/spec.md" }, role: context }
  - { unit: { kind: file, path: "specs/052-bitemporal-claim-history-and-projection/spec.md" }, role: context }
summary: >
  Retrieval quality is the product, and it degrades invisibly. This spec
  builds the instrument: a committed golden corpus of memories and queries
  with judged relevance, a runner that loads it into a temporary instance
  and executes every query, metrics that are the standard ones rather than
  invented, a baseline file per configuration, and a gate that fails a
  change which moves a metric down by more than its tolerance without an
  explicit, justified baseline update. Constitution XIV becomes a command
  with an exit code.
---

# 040: Evaluation

## 1. Purpose

Every parameter in this system is a guess until it is measured: chunk size
and overlap, the fusion weights and rank constant, the recency half-life,
the consolidation thresholds, the entity resolution threshold, and the
choice of embedding model. Without an instrument, each change is argued
from intuition and the system drifts.

The predecessor's community wrote smoke tests, health monitors, and world
model diagnostics for exactly this reason, after the fact and per fork
(`openbrain://valued-capabilities`).

## 2. Territory

The `aicortex-eval` crate and the `eval/` tree. The corpus is committed
data, not generated at run time.

B-11's capture adds a `query_capture` module and its tests to
`aicortex-store`, and reaches units other specs own only through
`extends` edges: 012's store root and migrations (the table), 014's
erasure (capture rows erased with the scope), 018's recall root (the
observation point), and 020's search handler (the HTTP recall path). Each of those files is claimed in its
owner's `establishes`; where the owner is still `implementation:
pending`, the file exists before this spec is built: each owner is in
this spec's `depends_on` closure (018 directly, 012, 014, and 020 through
035), which `scripts/spec-dag.sh` enforces.

## 3. Behavior

- **B-1 (the corpus).** `eval/golden/` holds memory sets and query sets in
  a documented format: each query has judged relevant memory ids with a
  graded relevance, and a note on why. It covers, at minimum: exact
  lookup, paraphrase, multi-hop through an entity, temporal preference
  where the newest of several similar memories is correct, negation,
  a query whose answer was superseded and must return the correction, and
  a query with no relevant memory at all where returning nothing is
  correct.
- **B-2 (synthetic scale).** A generator produces a large corpus with known
  structure for latency measurement, seeded and reproducible, kept
  separate from the judged corpus so scale numbers never contaminate
  quality numbers.
- **B-3 (metrics).** Recall at 5, 10, and 20; precision at 5; mean
  reciprocal rank; normalized discounted cumulative gain at 10; the
  no-answer rate on queries whose correct result is empty; and the
  truncation rate. Latency at p50, p95, and p99 per channel and fused.
  Definitions are the standard ones and are stated in `metrics.rs` with
  their formulas.
- **B-4 (the runner).** `aicortex-eval run --corpus <name> --config
  <profile>` boots a temporary instance through rahi's harness, imports
  the corpus, waits for embedding to drain, executes every query, and
  writes a report. It is hermetic: no network, the local provider, a fixed
  seed, and an injected clock.
- **B-5 (baselines).** `eval/baselines/<profile>.toml` records the last
  accepted metrics with the commit and date. `aicortex-eval check`
  compares a fresh run to the baseline and exits non-zero when any metric
  falls by more than its declared tolerance.
- **B-6 (the gate).** `make eval` runs `check` for the default profile.
  CI runs it when any file owned by 015, 016, 017, 018, or 034 changes,
  detected through the spec-spine index rather than a path glob. A change
  that moves a metric down must either be fixed or accompanied by a
  baseline update whose commit message states the tradeoff, which is a
  human decision visible in review.
- **B-7 (reports are diffable).** The report is deterministic text and
  includes per-query deltas against the baseline, so a reviewer sees which
  queries got worse rather than only that the mean moved.
- **B-8 (the corpus is honest).** Judgments record who made them and when.
  A query added because it failed is marked as such, so the corpus does
  not silently become a set of things that already pass.

### Method additions (2026-10-25, D-3)

The behaviors below widen the instrument without widening the gate. B-5
and B-6 stay hermetic and are the only gate. B-9 to B-12 produce reports
that are data for a human, and any part of them that needs a model runs
outside the gate, through governed egress, under 041.

- **B-9 (external benchmark adapter).** `aicortex-eval external --suite
  <name> --data <path>` loads a public benchmark into a temporary
  instance and reports its results in the benchmark's own terms, so a
  number can be compared with results published outside this project
  (`gbrain://external-benchmark`). LongMemEval is the reference suite: its
  sessions are imported as memories through the ordinary capture path,
  each question runs as a recall, and the hermetic part of the report is
  retrieval against the benchmark's labelled evidence sessions (recall at
  5, 10, and 20, and the B-3 metrics where they apply). Answer accuracy,
  which needs a reader model and a judge model, is an optional second
  stage under B-12's rules. The dataset is never committed: the operator
  supplies it, the adapter verifies it against a digest pinned in
  `eval/external/<suite>/`, and the report names the suite, its version,
  the digest, the configuration, and the commit. External results are
  never part of `check` and never update a baseline. A suite runs only on
  the temporary instance, never against a user's scope.
- **B-10 (preregistered sealed confirmation set).** Before any run of a
  confirmation set, a preregistration is committed to `eval/prereg/`: the
  hypothesis, the configurations compared, the primary metric, the sample
  size, the decision rule that maps a result to pass, fail, or
  inconclusive, and the expected headroom below the metric's ceiling. The
  queries and judgments of the set are sealed: they are held outside the
  repository, and only their digest is committed with the
  preregistration. `aicortex-eval confirm <prereg>` refuses to run when
  the preregistration was committed after the set's first recorded run,
  when the digest does not match, or when the set has already been run for
  that preregistration. The outcome is appended to the preregistration,
  including a fail, and the append is committed to the default branch,
  whose history is protected (merge queue only, no force push). A run
  record is that commit. "Committed after" is decided by ancestry on the
  default branch's first-parent history, never by an author or committer
  timestamp: the preregistration must be reachable from the commit before
  the earliest run record. `confirm` reads both from that history, never
  from the working copy, so a later commit that deletes or rewrites the
  record does not erase it. Once a set has been run it is unsealed and may
  only be
  used as development data afterwards. The rationale is
  `gbrain://sealed-confirmation-set`: a gain on development data that a
  sealed set does not confirm is a hypothesis, and a set whose baseline is
  already near its ceiling cannot confirm anything, which is why the
  expected headroom is part of the design.
- **B-11 (opt-in capture of real queries).** A scope's owner may enable
  capture of the queries recalled in that scope (`gbrain://eval-capture`).
  Capture is off by default. A captured row holds the query text after
  scrubbing, the filter, the time, and the trace's ids, ranks, and scores
  (018 B-6), never a memory body. Scrubbing is on by default and removes
  email addresses, telephone numbers, payment card numbers, and the
  shapes 013's secret detectors recognize; a query containing a detected
  credential is not captured at all. Scrubbing is pattern-based and is
  not general redaction: personal names, booking or order references,
  identity-document numbers, and other free-form identifiers can survive
  it. An operator may declare additional scrub patterns in configuration
  for identifiers specific to a deployment's domain; they apply in
  addition to the defaults and never replace them. Enabling capture therefore states that captured text may still hold
  personal data of the owner and of third parties, and the retention
  window is configurable down to one day. Capture never extends what the
  scope already holds to anyone new: the rows stay in the owner's own scope,
  under the same access and erasure as the owner's memories. Captured rows live in their own
  table in the scope's store, are never mixed with `eval/golden/`, are
  bounded by a retention window (default 30 days), and are erased with the scope
  (014). A captured query enters the judged corpus only by a human adding
  it with a judgment under B-8, after review of the scrubbed text. A
  capture failure is counted and never fails the recall it observed.
  Captured rows never leave the instance except by an explicit operator
  export to a local file; 041 governs this.
- **B-12 (contradiction probe).** `aicortex-eval contradictions --sample
  <n>` samples queries from the judged corpus or from captured queries,
  takes the top `k` of each recall, and asks a judge model whether any
  pair of returned memories contradicts on a claim relevant to the query
  (`gbrain://contradiction-probe`). The verdict is one of: no
  contradiction, contradiction, change over time, and not comparable; a
  change over time needs two different valid or capture times. Pairs
  already linked by a recorded supersession or correction (014), a
  surfaced contradiction (017 B-10), or a claim relation in one slot
  (052 B-5, B-6) are excluded, so the probe measures only contradictions
  the store has not recorded. The report gives the rate of queries with
  at least one contradiction, a Wilson 95% interval, and a small-sample
  note below 30 queries, as data and never as a gate. The probe writes
  nothing to the memory store. Every judgment records the judge model and
  version, the prompt version, and the time (B-8). The memory text sent to
  the judge is framed with 019's envelope, and the call goes through the
  kernel's governed egress facade, off by default (041).
- **B-13 (the probe complements 052).** 052 gives typed claims an explicit
  history, so a superseded value is known to be superseded. Prose memories
  outside 050 to 052 carry no such structure, and nothing tells the
  operator how often recall puts two incompatible statements side by side.
  The probe measures that rate. A high rate is evidence for moving more of
  a scope's content into typed claims or for curating it (034); a low rate
  says the explicit history and supersession already carry the load.

## 4. Functional requirements

- **FR-001.** `aicortex-eval run` on the committed corpus is deterministic:
  two runs produce identical reports.
- **FR-002.** `check` fails when fusion weights are perturbed enough to
  move recall at 10 below tolerance, and passes when they are unchanged.
- **FR-003.** Metric implementations match hand-computed values on a small
  fixture, asserted in `tests/metrics.rs`.
- **FR-004.** The runner completes the judged corpus within its time
  budget on CI hardware and reports latency percentiles.
- **FR-005.** The no-answer query returns an empty result, and returning
  anything scores zero for it.
- **FR-006.** The external adapter refuses a dataset whose digest does not
  match the pinned one, and over a committed miniature synthetic fixture in
  the LongMemEval layout (written for this repository, containing no
  LongMemEval content) reports the recorded retrieval metrics with no
  network.
- **FR-007.** `confirm` refuses a preregistration committed after the
  set's first run, a set whose digest differs, and a second run of the same
  preregistration.
- **FR-008.** With capture disabled, a recall writes no capture row. With
  it enabled, a query containing an email address is stored scrubbed, a
  query containing a fixture credential is not stored, no captured row
  contains a memory body, and erasing the scope removes every captured row.
- **FR-009.** With the judge disabled, the probe refuses to run and opens no
  socket. With a fake judge on a governed loopback host, every judgment
  records model, prompt version, and time; pairs already linked by a
  recorded supersession are never sent; and the memory store is unchanged
  after the run.

## 5. Acceptance criteria

- **AC-1.** `cargo test -p aicortex-eval --locked` passes.
- **AC-2.** `make eval` exits 0 against the committed baseline, and the
  baseline for the default profile is populated from a real run.

## 6. Out of scope

End-to-end quality of an assistant using this store, which depends on the
client. Benchmarking against other memory products, which is marketing
rather than a gate; B-9 runs a public dataset against this product so its
own results are comparable, and makes no claim about any other product.
Sending captured queries anywhere, which 041 B-4 forbids. Resolving a
contradiction the probe finds, which is review (023) or curation (034).

## 7. Resolved decisions

- **D-1 (2026-09-03, this spec).** The corpus is committed rather than
  generated. A generated corpus measures the generator's assumptions;
  judged data is the only thing that can contradict a design belief.
- **D-2 (2026-09-03, this spec).** The gate is triggered by ownership in
  the spec index rather than by a path glob, so a new file in a
  retrieval-owning spec is covered on the day it lands.

- **D-3 (2026-10-25, amendment, pending owner review).** Added B-9 to
  B-13 and FR-006 to FR-009: an external benchmark adapter, a
  preregistered sealed confirmation set, opt-in capture of real queries,
  and a contradiction probe, from four gbrain observations recorded in
  `docs/design/00-lineage.md`. The gate (B-5, B-6) is unchanged and stays
  hermetic; everything that needs a model is a report outside it, through
  governed egress under 041. Two parts of the observed system are not
  carried because they conflict with 041 B-4 (no telemetry): contributors
  shipping captured queries to a separate evaluation repository, and any
  capture path that leaves the instance. The amendment adds behavior to an
  approved spec that no human has reviewed, so this spec returns to
  `status: draft` (constitution V). 041 is amended in the same series for
  the egress and retention terms. The miniature fixture of FR-006 is
  synthetic, authored here in the benchmark's file layout and holding none
  of its content, so FR-006 does not depend on the dataset's licence; the
  dataset itself is never committed. Open for the owner: whether
  LongMemEval's licence allows its digest to be committed and results over
  it to be reported.

## Verification

```verify:cli
cargo test -p aicortex-eval --locked
```
