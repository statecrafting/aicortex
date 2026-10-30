---
id: "034-curation-workers"
title: "Curation: atomize, enrich, consolidate, expire, digest, all bounded and reversible"
status: draft
kind: "kernel"
domain: "memory"
created: "2026-09-03"
authors: ["Bartek Kus"]
implementation: pending
risk: high
wave: 3
depends_on:
  - "033-observatory-and-agent-ingestion"
establishes:
  - "crates/aicortex-curate/src/worker.rs"
  - "crates/aicortex-curate/src/atomize.rs"
  - "crates/aicortex-curate/src/consolidate.rs"
  - "crates/aicortex-curate/src/enrich.rs"
  - "crates/aicortex-curate/src/digest.rs"
  - "crates/aicortex-curate/src/schedule.rs"
  - "crates/aicortex-curate/tests/curate.rs"
  - "crates/aicortex-curate/testdata/curation/"
extends:
  - { spec: "023-review-and-promotion", unit: "crates/aicortex-curate/src/lib.rs", nature: additive }
  - { spec: "012-store-schema-and-repositories", unit: "crates/aicortex-store/src/migrations.rs", nature: additive }
references:
  - { unit: { kind: file, path: "specs/019-untrusted-content-boundary/spec.md" }, role: constraint }
  - { unit: { kind: file, path: "docs/design/00-lineage.md" }, role: context }
summary: >
  Without curation a memory store becomes a landfill: long notes that match
  nothing, near-duplicates from three sources, facts that stopped being
  true, and no way to see what a week contained. Five workers address that,
  all sharing one discipline: they run under a lease in bounded batches,
  every change they make is a lifecycle transition that is visible and
  reversible, they never delete, and anything they produce inherits the
  lowest trust class of its inputs. A curator can propose; only a human
  promotes.
---

# 034: Curation

## 1. Purpose

The predecessor's users wrote atomizers, consolidation workers, enrichment
passes, and daily and weekly digests, which says that a store which only
accumulates is not enough (`openbrain://valued-capabilities`). Curation is
also the only answer to near-duplicates, which exact fingerprinting cannot
catch (014 B-10).

The danger is a background process that quietly rewrites what the user
said. Every rule here exists to keep that from being possible.

## 2. Territory

Five worker modules, the scheduler, and their fixtures inside
`aicortex-curate`.

## 3. Behavior

- **B-1 (shared discipline).** Every worker acquires a rahi lease per
  `(scope, worker)` with its fencing token, processes a bounded batch,
  commits each unit of work in one transaction with its outbox effects,
  records progress so it is resumable, and reports counts on the event
  stream. No worker deletes anything: they supersede, expire, quarantine,
  or create.
- **B-2 (atomize).** A long memory is split into atomic memories, each a
  single claim, linked to the original by `derived_from`. The original is
  retained and superseded by the set, so a citation to it still resolves.
  Splitting is deterministic on sentence and section boundaries, with a
  minimum and maximum size, and is skipped for memories under the
  threshold.
- **B-3 (consolidate).** Near-duplicates within a scope are found by
  embedding similarity above a high threshold with the same memory kind,
  confirmed by a lexical check to avoid the failure mode where two
  different facts about the same topic look alike. A confirmed group is
  merged into the earliest memory, the others are superseded pointing at
  it, and all provenance rows are preserved. Below the confirmation
  threshold the group becomes a review item instead (023) rather than an
  automatic merge.
- **B-4 (enrich).** Enrichment fills structure the extractor missed:
  entities, edges, valid-until dates stated in text, and kind
  reclassification. It never edits the body. A reclassification records the
  prior value.
- **B-5 (expire).** The expiry pass of 014 B-5 runs here, plus a staleness
  pass that flags memories whose class has a natural half-life and that
  nothing has referenced in a long time, as review items rather than
  automatic expiry.
- **B-6 (digest).** A periodic digest per scope summarizes what arrived,
  what changed, what is waiting in review, and what contradicted itself.
  A digest is itself a memory of kind `Reference`, carries an envelope,
  and inherits the minimum trust class of its sources (019 B-5). Digests
  are opt-in per scope and delivered through the API and the event stream,
  never by mail from this product.
- **B-7 (reversibility).** Every curator action is a lifecycle transition
  with a recorded decision reference (024), and `POST /curation/{run}/undo`
  reverses a run within a retention window by restoring the superseded
  statuses. A curator that cannot be undone will not be trusted enough to
  be enabled.
- **B-8 (off by default, per scope).** Each worker is enabled per scope
  with its thresholds. A new deployment curates nothing until asked.
- **B-9 (no promotion, ever).** No curator can produce an `Instruction`
  memory; the type system prevents it (011 B-5) and a test asserts it.
- **B-10 (cost).** Workers run within a configured budget per scope per
  period, and a scope that exhausts its budget stops and says so rather
  than running unbounded work over a large store.
- **B-11 (retirement valve; added 2026-10-25, D-3).** A retirement is any
  transition a curator makes that removes a memory, an edge, or a derived
  item from active recall: supersede, expire, or quarantine. Each
  `(scope, worker)` has a valve: at most a configured number of
  retirements per window (default 100 per 24 hours), and, above a small
  floor (default 25), at most a configured share of the worker's own live
  outputs in the scope (default 20 percent). A run whose plan would cross
  either limit is held before it commits anything: it writes nothing,
  retires nothing, leaves the previous state live, records the held plan's
  counts, raises a review item (023), and emits an event. A held run is
  released only by an operator decision recorded in the ledger, which
  admits that one plan and does not raise the valve. The valve counts
  across runs in the window, so splitting a large retirement into many
  small runs does not pass it. It sits beside B-7's undo: the undo
  repairs a bad run after the fact, and the valve keeps a bad run from
  becoming large (`gbrain://retirement-valve`).
- **B-12 (an empty result is held without proof; added 2026-10-25,
  D-3).** When a worker recomputes what it derives from a source (a
  memory it atomized or enriched, or the inputs of a digest) and the new
  result is empty where the live result is not, the run is held exactly
  as in B-11, unless the source is proven empty. Proven empty means a fact
  in the store, not the absence of output: the source memory is no longer
  `Active`, or its body is normalized-empty, or a source adapter (030)
  recorded an explicit empty listing with its checkpoint. An extractor
  that failed, timed out, or returned nothing is never proof.
- **B-13 (a partial result retires nothing; added 2026-10-25, D-3).** A
  batch cut short by B-10's budget, by a lease loss, or by an input bound
  is partial. A partial result is applied additively: it creates what it
  produced and retires nothing, because a missing item in a partial result
  says nothing about the source.

## 4. Functional requirements

- **FR-001.** Over the committed curation fixtures, each worker produces
  the recorded transitions and no others.
- **FR-002.** Consolidation of three near-duplicates leaves one active
  memory, two superseded pointing at it, and three provenance rows.
- **FR-003.** A borderline duplicate pair produces a review item and no
  merge.
- **FR-004.** `undo` restores the exact prior statuses for a run and is
  itself recorded.
- **FR-005.** A digest built from `Assertion` sources has trust class
  `Assertion` and carries envelopes for every quoted memory.
- **FR-006.** A worker interrupted mid-batch resumes without repeating
  completed units.
- **FR-007.** No curator path can construct an `Instruction` trust class
  (compile-fail test).
- **FR-008.** A fixture run planning one more retirement than the valve
  allows writes nothing, leaves every prior status unchanged, and raises
  one review item; two runs in one window whose sum crosses the valve are
  treated the same way on the second run; an operator release admits that
  plan once.
- **FR-009.** An enrichment re-run whose extractor returns no entities for
  a memory that still has live edges is held; the same re-run after the
  memory is superseded retires the edges.
- **FR-010.** A batch interrupted by budget exhaustion creates its outputs
  and retires nothing.

## 5. Acceptance criteria

- **AC-1.** `cargo test -p aicortex-curate --locked` passes.
- **AC-2.** On a 50000-memory fixture scope, a full curation cycle
  completes within its budget and the evaluation corpus (040) shows recall
  no worse than before the cycle.

## 6. Out of scope

Summarization with a remote model, which would be an egress addition and a
separate decision. Cross-scope consolidation, which would breach scope
isolation (012 B-9).

## 7. Resolved decisions

- **D-1 (2026-09-03, this spec).** Consolidation merges only above a high
  confirmed threshold and refers everything else to review. An aggressive
  merge destroys distinctions the user made deliberately, and the user
  cannot tell it happened.
- **D-2 (2026-09-03, this spec).** Digests are pulled, not mailed. Adding
  outbound mail would add an egress host, a template surface, and a
  deliverability problem to a product whose value is local.

- **D-3 (2026-10-25, amendment, pending owner review).** Added B-11 to
  B-13 and FR-008 to FR-010: a retirement valve per worker and window, a
  hold on empty results without proof that the source is empty, and an
  additive rule for partial results. The observation is
  `gbrain://retirement-valve` (`docs/design/00-lineage.md`): a derived
  writer that is held, writing nothing, when one window would retire too
  much, and that never treats an empty result as proof of an empty source.
  B-7's undo already makes a bad run reversible; these rules bound how much
  damage a run can do before a human sees it, which matters most for the
  failure B-7 cannot see, an extractor that silently returns nothing. The
  defaults are guesses to be measured, not tuned values. The amendment adds
  behavior to an approved spec that no human has reviewed, so this spec
  returns to `status: draft` (constitution V).

## Verification

```verify:cli
cargo test -p aicortex-curate --locked
```
