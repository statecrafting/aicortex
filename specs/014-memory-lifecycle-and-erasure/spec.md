---
id: "014-memory-lifecycle-and-erasure"
title: "Lifecycle: supersession, correction, expiry, deduplication, and erasure that reaches the vectors"
status: approved
kind: "kernel"
domain: "memory"
created: "2026-09-03"
authors: ["Bartek Kus"]
implementation: complete
risk: critical
wave: 1
depends_on:
  - "013-write-gate-and-redaction"
establishes:
  - "crates/aicortex-store/src/lifecycle.rs"
  - "crates/aicortex-store/src/fingerprint.rs"
  - "crates/aicortex-store/src/erasure.rs"
  - "crates/aicortex-store/tests/lifecycle.rs"
  - "crates/aicortex-store/tests/erasure.rs"
extends:
  - { spec: "012-store-schema-and-repositories", unit: "crates/aicortex-store/src/lib.rs", nature: additive }
  - { spec: "012-store-schema-and-repositories", unit: "crates/aicortex-store/src/migrations.rs", nature: additive }
  - { spec: "012-store-schema-and-repositories", unit: "crates/aicortex-store/src/memory_repo.rs", nature: additive }
  - { spec: "012-store-schema-and-repositories", unit: "crates/aicortex-store/Cargo.toml", nature: additive }
  - { spec: "012-store-schema-and-repositories", unit: "crates/aicortex-store/tests/common/", nature: additive }
  - { spec: "012-store-schema-and-repositories", unit: "crates/aicortex-store/tests/repo.rs", nature: additive }
  - { spec: "012-store-schema-and-repositories", unit: "apps/aicortex/tests/migrate.rs", nature: additive }
  - { spec: "010-chassis-adoption-and-workspace", unit: { kind: section, file: "Cargo.toml", anchor: "workspace.dependencies" }, nature: additive }
  - { spec: "011-memory-model", unit: "crates/aicortex-types/src/lib.rs", nature: additive }
  - { spec: "010-chassis-adoption-and-workspace", unit: "apps/aicortex/tests/cell.rs", nature: additive }
constrains:
  - { flavor: invariant-freeze, unit: "crates/aicortex-store/src/erasure.rs", note: "erasure reaches every derivative; memory content never enters the ledger" }
  - { flavor: invariant-freeze, unit: "crates/aicortex-store/src/erasure.rs", note: "erasure destroys the B-9 digest key of every Decision about the erased memory or scope" }
references:
  - { unit: { kind: file, path: "specs/013-write-gate-and-redaction/spec.md" }, role: constraint }
summary: >
  A memory store that can only append becomes wrong over time and then
  confidently recalls the wrong thing. This spec gives a memory the rest of
  its life: an exact fingerprint that merges a repeated capture instead of
  duplicating it, supersession that keeps the old row readable but
  unretrievable, correction that records who said the old value was wrong,
  expiry for memories that were true until a date, and erasure that removes
  content, chunks, embeddings, and index entries in one transaction while
  the ledger records only that it happened.
---

# 014: Lifecycle

## 1. Purpose

The predecessor deduplicated on exact normalized text and had no other
lifecycle at all, so near-duplicates accumulated without bound and a fact
that stopped being true stayed retrievable forever
(`openbrain://exact-dedup-only`). Confidently recalling a stale fact is the
characteristic failure of memory systems and the one users notice.

This spec also resolves the tension constitution XIII names: the decision
chain is append-only and hash-linked, and a user has a right to erasure.
Both are satisfiable at once, provided content never enters the chain.

## 2. Territory

Three modules and two tests inside `aicortex-store`, plus the migrations
they add. Extends 012's `lib.rs` and migration list.

This spec also carries spec 013's erasure obligation, because `erasure.rs`
is this spec's file to write. Spec 013 B-9 mints a keyed digest per
Decision and holds the key in an application-store row; the property that
erasure destroys those keys is frozen here, on the unit that implements it,
and is discharged by B-11 and FR-007 (amended 2026-09-17, D-2). Spec 013
FR-008 forbids any surface or document from claiming that erasure reaches
refusal and quarantine records until FR-007 below shows it, and that
prohibition stays 013's.

## 3. Behavior

- **B-1 (fingerprint).** `fingerprint(scope, normalized_text) -> [u8; 32]`,
  a BLAKE3 hash over the gate-normalized body with the scope mixed in.
  Unique among non-erased rows per 012 B-2.
- **B-2 (merge on repeat).** A capture whose fingerprint already exists in
  the scope does not insert. It merges: `updated` moves forward,
  `importance.uses` increments, the new provenance is appended as an
  additional source row, and any new metadata is unioned. The caller is
  told it was a merge and receives the existing id. This makes a repeated
  import idempotent by construction.
- **B-3 (supersession).** `supersede(old, new)` sets the old row to
  `Status::Superseded(new)` in the same transaction that inserts the new
  one. Superseded rows are excluded from retrieval, retained for audit,
  and readable by id. A supersession chain is walkable in both directions
  and is asserted acyclic on write.
- **B-4 (correction).** A `Correction` memory records that a prior memory
  was wrong, names it in `derived_from`, and supersedes it. The correction
  carries the actor who made it. A correction by an agent is an
  `Assertion`; only a human correction can be promoted further.
- **B-5 (expiry).** A memory may carry `valid_until`. A curator pass moves
  expired rows to `Status::Expired` in bounded batches under a rahi lease
  with its fencing token. Expiry is a status change, never a delete.
- **B-6 (decay is read-time).** Importance decays by computation at read
  time (011 B-9). No background job rewrites rows to decay them, because
  that would be a write amplification proportional to the store size on
  every tick.
- **B-7 (erasure).** `erase(scope, id, authority)` deletes, in one
  transaction: the body, every chunk, every embedding, every index entry,
  every derivation row naming it as a child, and its provenance detail. It
  retains the row shell with `Status::Erased`, the id, the scope, and the
  timestamps, so that references from other memories resolve to a
  tombstone rather than dangling. It appends one ledger Decision naming
  the scope, the id, the authority, and a count of removed derivatives,
  and never the content or its hash preimage.
- **B-8 (erasure of derived memories).** Erasing a memory does not
  automatically erase memories derived from it, because a derived summary
  may be independently valuable, but it marks each derivative
  `origin_erased` and makes that visible in recall traces. Cascading
  erasure is an explicit flag on the call and is reported in the Decision.
- **B-9 (scope erasure).** `erase_scope` is the same operation over every
  memory in a scope, run in bounded batches under a lease, resumable, and
  reported by a single Decision at completion with the per-batch progress
  in the work journal.
- **B-11 (digest keys).** Erasure destroys the B-9 digest key of every
  Decision about the erased memory or scope. This is spec 013 B-9's
  requirement, frozen on this spec's unit and carried here verbatim
  (amended 2026-09-17, D-2): erasing a quarantined memory (B-7) destroys
  its Decision's key, and erasing a scope (B-9) destroys every key in the
  scope, in the same transaction as the rest of the erasure. The staging
  calls exist already, as `DecisionKeyRepo::stage_destroy_for_memory` and
  `stage_destroy_for_scope` in the `decision_key.rs` spec 013 established;
  what this spec adds is that `erase` and `erase_scope` call them and that
  FR-007 proves they did. A destroyed key leaves the chained digest an
  opaque value that cannot be confirmed against a guessed body, which is
  what lets an append-only chain and constitution XIII both hold.
- **B-10 (near-duplicates are not handled here).** Semantic deduplication
  requires embeddings and is a curator concern (034). This spec handles
  exact repeats only, and says so rather than half-solving it.

## 4. Functional requirements

- **FR-001.** Capturing the same text twice in one scope yields one row,
  two provenance rows, and `uses == 2`; capturing it in another scope
  yields a separate row.
- **FR-002.** A superseded memory is absent from a retrieval result and
  present in a by-id read; the chain from new to old is walkable.
- **FR-003.** After `erase`, a targeted search that previously returned
  the memory returns nothing, and a direct read of the embedding and chunk
  tables for that id returns zero rows.
- **FR-004.** The ledger Decision emitted by `erase` contains no substring
  of the erased body, asserted over a fixture whose body is a distinctive
  token.
- **FR-005.** `erase_scope` over 5000 memories completes in bounded
  batches, is resumable after an induced crash, and leaves no orphaned
  embedding or chunk row.
- **FR-006.** A supersession that would create a cycle is rejected.
- **FR-007.** The three things spec 013 FR-008 requires before erasure may
  be claimed to reach refusal and quarantine records, each asserted rather
  than described. After erasing a quarantined memory, and after erasing a
  scope, no `decision_key` row covering the erased object remains and
  `DecisionKeyRepo::get` returns `None` for the key id the Decision names.
  A backup taken after the erasure holds no such key. No replay path
  restores one: neither redriving the outbox, nor re-importing the same
  content, nor a capture of the same body mints a row under the destroyed
  key id. The result of an erasure says in so many words that a backup
  taken *before* it still holds the key until that backup is discarded,
  rather than implying otherwise.

## 5. Acceptance criteria

- **AC-1.** `cargo test -p aicortex-store --locked --test lifecycle` and
  `--test erasure` pass.
- **AC-2.** An erasure followed by `aicortex ledger verify` leaves the
  chain intact.

## 6. Out of scope

Semantic near-duplicate consolidation and summarization (034). The review
queue that promotes out of quarantine (023). Retention policy defaults,
which are deployment configuration.

## 7. Resolved decisions

- **Status (2026-09-26, completion on released rahi 0.4.0).** All nine rahi
  crates resolve at exact registry version 0.4.0 from the crates.io index,
  whose signed `v0.4.0` tag peels to
  `e845bd08f3b73a17f19656accb3c2ab24975f277`. The locked graph contains
  `hiqlite-patched` and `hiqlite-wal-patched` 0.15.0-patched.3 with no path,
  Git or `[patch]` override. H-2 now exercises a stale release after TTL
  takeover without a fenced write, then proves unrelated and same-key lock
  progress. H-3 stops a full node while its lease is held, reopens the same
  durable state at identical Raft and API listener identities, waits
  `LEASE_TTL_SECONDS + 1`, submits a fresh erasure request, and proves the
  operation completes exactly once. The declared verification, all store and
  migration tests, the literal CLI acceptance test, the registry-only
  resolution check, and the full repository gate and CI pass. The lifecycle
  is therefore complete; earlier status entries remain as the chronological
  record of why it could not complete on older chassis releases.

- **D-16 (2026-09-26, owner work order; one neutral time representation).**
  The public lifecycle API names `aicortex_types::AicortexTime`, an alias of
  rahi's `UnixSeconds`, for `valid_until` and expiry comparison. This closes
  the representation gap without adding a travel type, a second clock, or a
  field to 011's serialized `Memory`: D-3's column-versus-record decision
  remains intact. Its public documentation links to `TimeValue`, the public
  source-time type, so the separation is expressed through a resolvable API
  item rather than a module name. Rejected: 050's civil and zoned source-time
  types, which do not represent record-clock deadlines, and a new wrapper
  with conversion semantics distinct from the chassis clock.

- **Status (2026-09-25, port onto main and chassis re-check).** The work of
  pull request #15 (last commit `eae4258`, based on `a927f85`) is carried
  onto main after 046 (rahi 0.2.0), 050 and 051, on a new branch; #15 is
  superseded. Of the three prerequisites of the 2026-09-19 entry:

  - H-1 (lifetime-idempotent ledger) **is met**. Main pins rahi 0.2.0,
    which carries rahi 042. The two diagnostics that reproduced the
    archived-retry duplicate now fail as diagnostics, because the duplicate
    no longer occurs, and are converted to positive assertions (D-13). The
    third, `pinned_archive_failure_is_not_proven_absence`, still passes: the
    receipt path still reads resident history only, and moving it onto
    `Ledger::lookup`'s five answers (handoff section 4, step 2) is not done
    here.
  - H-2 (the stale-lease-release panic) **is not met**. rahi 0.2.0's
    `Lease::release` suppresses a release only after a fenced write has
    detected supersession, as 0.1.0 did, and rahi's 0.3.0 changelog says a
    crates.io consumer "gets hiqlite 0.14.0 without that fix unless it
    declares the same section": a `[patch.crates-io]` git source, which
    010 D-1 and D-2 and `deny.toml` (`allow-git = []`) refuse. The newest
    published hiqlite, 0.15.0, is a semver-incompatible line no rahi
    release depends on. Resolving this is an owner decision (the patch
    against 010 D-1, or waiting for a rahi release on a fixed hiqlite,
    rahi 043), not a build-session one.
  - H-3 (restart, then a second lease acquisition) is the known hiqlite
    limitation the owner recorded on 2026-09-25: a lease held at a crash
    reads as held until `LEASE_TTL_SECONDS` expire, and restart tests wait
    that TTL plus one second. No test here yet exercises a full-node
    restart with a lease held.

  Both acceptance criteria hold on this tree: AC-1 (`--test lifecycle`,
  15 passed, and `--test erasure`, 13 passed and 1 ignored), and AC-2
  through that ignored test, `ac2_literal_cli_verifies_a_real_erased_fixture`,
  run with `AICORTEX_VERIFY_BINARY` set to the `aicortex` binary built from
  this tree (1 passed).
  014 stays `implementation: in-progress` because H-2 is a reproduced
  crash of the lock handler on the lease-handoff path B-9 relies on, and
  the 2026-09-17 entries make its repair a completion prerequisite. 052
  depends on 014 and stays blocked with it.
- **D-13 (2026-09-25, build session; the ledger diagnostics become
  assertions).** `pinned_archived_retry_duplicates_after_reopen` becomes
  `archived_retry_appends_one_copy_after_reopen` (one copy across the
  sealed segment and the resident chain, none resident after the retry),
  and `pinned_verified_lookup_does_not_fence_concurrent_append_and_seal`
  becomes `concurrent_append_and_seal_keep_one_copy` (the rival append
  returns the original hash; history holds one copy). The fixtures, faults
  and bounds are unchanged; only the expected outcome moves from the
  defect to its repair.
- **D-14 (2026-09-25, build session; migration numbers and additivity).**
  Main shipped versions 4 (050's predicate registry) and 5 (051's claim
  admission) while #15 was open, so #15's two migrations become 6
  (`LIFECYCLE_VERSION`) and 7 (`ERASURE_RECEIPTS_VERSION`); neither was
  ever applied to a store from main, so no shipped SQL changes (046 B-3).
  Per 046 B-2, version 6 is declared additive: it adds nullable or
  defaulted columns, one index and two tables, and an older binary reads
  and writes the memory row without them. Version 7 is left undeclared:
  it carries the fingerprint version the re-digest of B-1 (D-7) advances,
  which 046 B-2 names as not additive, and an older binary serving a store
  ahead of it would keep minting the old digest beside the new one. 046's
  migrate test (`apps/aicortex/tests/migrate.rs`) asserted every shipped
  migration additive; it now asserts the exact declared flag per version,
  with version 7 the one undeclared. The migration test in
  `tests/lifecycle.rs` moves from `[..4]`, 4 to 5, to `[..6]`, 6 to 7.
- **D-15 (2026-09-25, build session; positional placeholders).** Seven of
  this spec's statements named a `$n` twice (five in `erasure.rs`, two in
  `lifecycle.rs`), each in ascending first-appearance order and so bound
  correctly; they use `?NNN` now, which 012 D-9's scan test requires of
  every statement in the crate.

- **Status (2026-09-19, prerequisite re-check).** The three chassis
  prerequisites the entries below leave open were re-read against the
  registry and the sibling checkout rather than against the earlier
  reports, and all three are still unmet.

  The crates.io index carries exactly one version of every rahi crate,
  `0.1.0`, and the only tag in `statecrafting/rahi` (local and on the
  remote) is `v0.1.0`. Spec 042, the lifetime-identity work of rahi PR #65,
  is `implementation: complete` on rahi's `main` at `9b38b34` and is in no
  published release, so the chassis API this spec's archived-retry blocker
  needs cannot be depended on yet. rahi's own consumer contract states the
  same: "implemented in the repository and published in no release", and
  "every published version still behaves as the limitation describes".

  The stale-lease-release repair is upstream hiqlite PR #352, merged and
  unreleased; the newest published hiqlite is `0.14.0` of 2026-07-06, which
  predates the merge. rahi obtains the fix only through a
  `[patch.crates-io]` at its workspace root (rahi 011 D-12), which is
  workspace metadata that no consumer inherits over either a registry or a
  git dependency edge. This repository's registry-only policy (010 D-1,
  D-2) stands, so a green run inside rahi's patched workspace is evidence
  about that workspace and is not read here as evidence about a published
  consumer.

  Full-node restart followed by a second lease acquisition remains
  unverified in rahi as well: its consumer contract names it as an open
  limitation, and the restart property in its evidence table is proven only
  as a reboot on a restored volume. That is application snapshot recovery,
  which is the same thing FR-007's recovery evidence already exercises
  here, and it is not the cached-lease property
  `data/014-full-node-restart-blocker.log` records as blocked.

  Contradictory release documentation was reconciled rather than averaged:
  rahi's `README.md` heading calls 0.2.0 a release candidate and its
  consumer contract cites a `v0.2.0` source tree, while the same documents
  say plainly that 0.2.0 is unpublished, that the tag link "does not assert
  that the tag or release exists yet", and that a version declaration is
  not publication. The registry and the remote tag list settle it: nothing
  past 0.1.0 exists to depend on.

  Consequently no chassis adoption, pin change, API migration or blocker
  conversion is performed in this session. The remaining chassis
  implementation, validation and release work is written up as a handoff in
  `docs/design/02-rahi-chassis-prerequisites-handoff.md`, naming the rahi
  spec that owns each item. 014 stays `implementation: in-progress`, 015
  stays blocked, and the diagnostics below stay diagnostics.

- **Status (2026-09-17, archived retry diagnostic).** D-12's no-duplicate
  claim is disproved beyond the resident window. A bounded regression on
  the actual pinned ledger appends an erasure Decision, interrupts delivery
  before receipt acknowledgement, seals genesis and that Decision through
  `Ledger::seal_if_needed` into `FsArchive`, reopens the same durable node,
  and retries. The one-copy assertion failed: two copies, not one. Both
  copies carry identical full Decision content except their assigned chain
  parent, including the original authority and actual counts. The tombstone
  timestamp is unchanged, but the receipt is marked delivered despite the
  duplicate. Full-chain verification still succeeds. The initial failing
  run is retained in `data/014-archived-retry-before.log`.

  Three `pinned_` diagnostic tests now preserve the counterexamples and
  archive evidence. Their success means the blocker is reproduced, not that
  B-7 or B-9 is satisfied. The healthy archive permits full-content recovery
  for inspection through the chassis verifier and segment APIs. Missing,
  corrupt and unreadable archive bodies return `NotFound`, `Integrity` and
  `Io`, respectively; resident-only lookup reports absence in all cases.
  A deterministic concurrent test pauses a retry after a complete verified
  negative lookup, appends and seals the same Decision in another task,
  then resumes the retry: both appends succeed at different hashes. Every
  test has a 30-second bound; none writes chassis tables or uses a fake
  ledger. Only a disposable archive object is damaged for refusal testing.

  This is a diagnostic-only change. Adding an archive scan or checking
  segment counts before append would still leave that demonstrated race.
  A process-local lock cannot cover another node or sealing. An application
  attempt marker could refuse recovery but cannot make the chassis's own
  append retry atomic with sealing, and refusing every uncertain receipt
  does not fulfill resumable delivery. No new refusal policy, successful
  archived delivery, or lifetime-idempotence repair is claimed. The concrete
  chassis need is an atomic lifetime-idempotent append/recovery API that
  keeps Decision identity across sealing, compares all content except the
  assigned parent, returns the original result for an identical retry,
  rejects conflicting content, and distinguishes unavailable or corrupt
  archive evidence from proven absence while handling concurrent appends
  and sealers. Existing receipts must remain recoverable through that API.
  No ratified requirement is changed. 014 stays in progress, 015 stays
  blocked, and the full-node restart and stale-release/TTL prerequisites
  above the application remain blocked pending published chassis repair.

- **D-12 (2026-09-17, durable review remediation).** D-6's post-commit
  ordering stays intact, but its claim that a retry could report a lost
  erasure needed durable state. Schema migration 5 adds an erasure receipt
  carrying the original authority, request metadata and stable Decision id.
  Destruction, its actual-row counts and receipt readiness commit together.
  Delivery happens afterwards through the chassis ledger. A retry compares
  the resident Decision's full content, excluding its assigned chain parent,
  before acknowledging delivery. A failed append leaves a discoverable
  receipt; a successful append with a lost acknowledgement is not appended
  twice. Completed single-memory retries return the original receipt instead
  of rejecting the tombstone. The former refusal test now asserts unchanged
  timestamps, one Decision, and the same result; cross-scope refusal stays.

  Scope receipts identify one operation across restarts. Each batch captures
  SQLite's actual affected-row counts immediately after each mutation inside
  the same transaction, including destroyed keys and refusal-only work. The
  completion Decision uses those durable operation totals and the original
  authority. A completed operation refuses a stale batch at commit; new
  content after delivered completion starts a new operation with separate
  totals. Accounting increases statement volume, so transactions take at
  most 200 rows within the caller's ceiling of up to 500. A 500-row accounted
  transaction exceeded the pinned engine's 2 MiB WAL entry limit in the
  FR-005 fixture; 200-row transactions pass that same 5000-memory fixture.
  This row bound does not establish or change lease safety.

  D-7's backfill now stores a fingerprint version with each digest. Reads
  select only older versions in id order. Digest and version commit together;
  a changed record aborts the batch and an erased row cannot be restored.
  Concurrently consumed batches do not report convergence while older
  versions remain. Migration 5 extends migration 4 without rewriting it.

  Rejected: detached in-memory receipts, journaling counts after destruction,
  rebuilding totals from tombstones, and repeatedly selecting the first
  nonempty fingerprints. The repair cannot reconstruct counts or authority
  already lost by the previous implementation. Legacy scope journal rows are
  retained and require reconciliation rather than being reported as known
  zero key destruction. These are implementation choices satisfying B-7,
  B-9, B-11 and D-7; no approved requirement is changed.
- **Status (2026-09-17, durable review remediation).** Single-memory tests
  reopen the same node after journal rollback, destructive commit, ledger
  failure, and ledger append before delivery acknowledgement. Scope tests
  restore real app-database snapshots into fresh nodes at those boundaries,
  asserting exact totals, including refusal-only keys, and one Decision.
  A full-node scope restart diagnostic remained blocked after asynchronous
  release: both the rollback and completed-operation cases retained
  `lease_fence.token = 1` while the next call waited to acquire its second
  lease. The run was terminated, not passed. Evidence is retained in
  `data/014-full-node-restart-blocker.log`. Cached-lease restart safety must
  be verified alongside the known TTL-release blocker; the app-snapshot
  tests do not claim it. The backfill test spans multiple batches, a rolled-back batch,
  repeated reopen, and zero-work convergence. The literal current
  `aicortex ledger verify` command has also run against a real erased fixture
  with the cell's own genesis: exit 0, two resident records, no sealed
  segments. The separate stale-lease release blocker below is unchanged:
  014 remains in progress, 015 remains blocked, and a published chassis fix
  and reviewed pin update are still prerequisites to completion.

- **D-11 (2026-09-17, review remediation).** D-9's claim that every
  erasure statement was idempotent was false for counter deltas and for
  tombstone timestamps. Counter moves now select the current live row
  inside the erasure transaction, before a guarded tombstone update. A
  second batch built from the same earlier read moves no counter and does
  not replace the first tombstone. A concurrent expiry moves the source
  bucket before erasure observes it, so erasure decrements that bucket.
  Tests force both commit orders against the real store rather than hoping
  concurrent tasks overlap. A key-only final batch also records its actual
  destruction in the journal and completion Decision.

  The same review found that a staged merge could restore an erased body,
  and a staged supersession could move counters after erasure. These
  read-modify-write paths compare the record at commit and abort the whole
  transaction on disagreement, including source, successor and outbox
  inserts. Supersession also rechecks reachability inside the transaction,
  because two individually acyclic reads can otherwise commit a cycle.
  Rejected: a zero-row update with unconditional follow-on writes, or a
  lock assumed to protect a transaction the caller has not committed yet.
  These are implementation repairs to B-2, B-3 and B-7, not changes to
  their requirements.
- **Status (2026-09-17, review remediation).** The SQL overlap defects are
  repaired, but the broader lease-handoff safety claim in D-9 remains
  unproved and has a reproduced chassis blocker. With pinned rahi 0.1.0
  and hiqlite 0.14.0, acquire a lease, wait 11 seconds, acquire a newer
  lease on that key, and release the old lease without a fenced write.
  The lock handler panics at `dlock_handler.rs:154`; acquiring a lease on
  another key then times out. `erase_scope` uses ordinary transactions
  and releases its lease this way. The local diagnostic failed with exit
  101; its log is retained in untracked
  `data/014-stale-lease-diagnostic.log`.

  `rahi_store::Lease` suppresses stale release only after `fenced_txn`
  detects supersession. That API accepts only UPDATE and DELETE against
  tables carrying a fence column; the erasure transaction also contains
  counter upserts and tables without that column. A separate fencing probe
  would leave a race before release and is not an atomic repair. The next
  action is a chassis fix and release supporting safe lease handoff, then
  a reviewed pin update and a rerun of 014 acceptance including TTL
  takeover. No chassis fork, shared-binary replacement, waiver, or contract
  relaxation is adopted here. Keep 014 in progress and do not start 015.

  FR-007 recovery evidence now boots each real pre- or post-erasure app
  database snapshot into an isolated rahi store. The former restores the
  original digest key, as the required caveat says; the latter retains
  tombstones and no destroyed key, including after outbox redrive and
  exact-body recapture. This exercises app-store snapshot recovery, not
  the operator's encrypted archive or identity-store restore.
- **D-2 (2026-09-17, amendment, human-authorized).** Spec 013 originally
  froze "erasure destroys the B-9 digest key of every Decision about the
  erased memory or scope" on `crates/aicortex-store/src/erasure.rs`, a file
  this spec establishes and which does not exist until this spec is built.
  That forward `constrains` edge made 013 unable to reach
  `implementation: complete` without a blocking `I-004`: the corpus
  contract makes an unresolved *owned* unit an error at the `complete`
  tier, and `constrains` is an owning edge there. The maintainer authorized
  this specific amendment on 2026-09-17: move the constraint to the spec
  that establishes the unit, preserving the requirement and its
  traceability, and explicitly not a general relaxation of `constrains`
  validation.

  The invariant is carried here verbatim, on the same unit, in this spec's
  `constrains` list. What the move adds is enforcement a note by itself
  could not give: B-11 makes the obligation a behavior of this spec, and
  FR-007 makes it evidence this spec's acceptance must produce, so
  `spec-spine verify 014` fails if `erase` and `erase_scope` do not destroy
  the keys. The `references` edge to `specs/013-write-gate-and-redaction/
  spec.md` with `role: constraint` is the corpus's existing idiom for "this
  spec is bound by that one" (spec 045 uses it against 013 already), and it
  keeps 013 surfaced as an authority over this work. Spec 013 keeps FR-008,
  which forbids claiming the erasure reaches these records until FR-007
  shows it.

  Rejected: a placeholder `erasure.rs` in 013's session, which would have
  put this spec's file in another spec's change; implementing this spec
  early to make the unit exist, which is a second spec in one session; a
  `Spec-Drift-Waiver:`, which records a contradiction rather than resolving
  it; and amending the contract's lifecycle table to exempt `constrains`
  generally, which the maintainer explicitly declined to authorize.
- **D-3 (2026-09-17, build session).** `valid_until` (B-5) is a column this
  spec adds to the memory table, not a field of the record. The record of
  011 has no such field, and adding one would amend another spec's type and
  bump the record's schema version for a value no reader of the record
  needs. It is read by the expiry predicate and by nothing else, which is
  exactly the test 012 D-3 sets for a column existing at all. The same
  reasoning covers `origin_erased` (B-8) and the `fence` column the chassis
  requires on any table a leased pass writes.
  Rejected: a field on `Memory`, which is 011's to add; and a side table
  keyed by memory id, which would make the sweep a join for no gain.
- **D-4 (2026-09-17, build session).** FR-001's "two provenance rows" are
  two rows of a new `memory_source` table, one per capture, and *not* two
  rows of 012's `provenance` table. 012 B-2 says `provenance` is one row
  per memory and that stays true: that row is the projection of the
  provenance the record carries, and a merge does not change what the
  record says about its own origin. What a merge does create, for the first
  time, is a second *source* for one row, which is the words B-2 uses
  ("appended as an additional source row"). Without the table the second
  capture's origin would simply be lost, which is the thing constitution IX
  forbids.
  Rejected: rebuilding `provenance` without its primary key, which would
  rewrite a sibling spec's table shape to satisfy a reading its own text
  contradicts.
- **D-5 (2026-09-17, build session).** B-7 requires erasure to reach every
  chunk, embedding and index entry. Those tables belong to specs 015 and
  016 and do not exist at this ordinal, and this spec does not create them:
  015 section 2 and 016 section 2 name them as their own territory. The
  sweep is a declared list of derivative tables in `erasure.rs`
  (`DERIVATIVES`) plus a second list naming the ones still to come
  (`PLANNED`), which 015 and 016 move across in the same change as the
  migration that creates them, under an `extends` edge on that file.
  FR-003 is asserted, not deferred: the test creates the future tables
  under their declared names and columns, registers them with
  `Eraser::also`, and reads zero rows for the erased id afterwards, having
  first read one row for each before.
  Rejected: creating the tables here, which would take another spec's
  territory; and sweeping them unconditionally, which would fail every
  erasure until 015 lands, because SQLite rolls the whole transaction back
  on a `DELETE` against a table that does not exist.
- **D-6 (2026-09-17, build session).** The erasure's Decision is appended
  *after* its transaction commits, not before and not inside it. The chain
  is append-only, so a Decision appended first and then not carried out
  would be a permanent and unretractable claim that content was destroyed
  when it was not. The other order loses, on a crash between the two, the
  record of an erasure that did happen; a rerun reports it and nobody is
  ever misled about what the store holds. The chain and the application
  store are separate commit domains and the chassis offers no transaction
  spanning them, so one of the two orders had to be chosen.
- **D-7 (2026-09-17, build session).** The re-digest 012 D-4 promised this
  spec would carry is a bounded backfill (`Lifecycle::redigest`), not a SQL
  migration. BLAKE3 has no spelling in SQLite, so the digest of B-1 cannot
  be computed by a migration at all; it is computed in process from each
  row's own record, which is the source of truth for what a memory says
  (012 D-3), and written back in one transaction. It is resumable by
  construction and converges when it reports zero.
- **D-10 (2026-09-17, build session; accepted by the maintainer
  2026-09-19).** B-1 mixes the
  scope into the content digest, and that changed the value spec 012's
  `tests/repo.rs` reads in
  `b3_b9_fr003_a_read_for_one_scope_never_returns_another`. That test
  computed one fingerprint from alice's row and asserted it also resolved
  bob's, which was true only because the placeholder digest 012 D-4
  installed did not mix the scope in. 012 D-4 says in so many words that
  "014 replaces the function", so the change of value is authorized; what
  is not automatic is touching a sibling spec's test, so it is recorded
  here and declared as an `extends` edge on that unit.
  The adaptation is strictly stronger on the invariant the test names.
  Before: one assertion, that each scope resolves a holder for the shared
  value. After: that the two scopes' fingerprints differ, that each
  resolves its own, *and* that neither resolves the other's. Nothing that
  was asserted has stopped being asserted; two refusals were added.
  This is flagged rather than quietly done because a build session adapting
  another spec's test is the shape of a weakened gate even when it is not
  one, and a human should confirm the reading of 012 D-4.

  **Accepted 2026-09-19 (maintainer).** The look this entry asked for was
  taken and the adaptation is accepted as narrow. The fingerprint of B-1
  includes the scope, and
  `b3_b9_fr003_a_read_for_one_scope_never_returns_another` retains both
  halves of the property it names: each scope resolves its own holder, and
  neither scope resolves the other's, alongside the by-id, listing and
  provenance refusals it already carried. No acceptance criterion of this
  spec or of 012 changes, no requirement is relaxed, and 012 D-4's reading
  is unchanged; the `extends` edge on
  `crates/aicortex-store/tests/repo.rs` remains the record of the touch.
- **D-9 (2026-09-17, build session).** `erase_scope` holds one lease for the
  whole drain rather than re-acquiring one per batch. B-9 says "under a
  lease", and one lease is the reading that works against this chassis
  version: hiqlite releases a lock on a task of its own and acknowledges
  nothing (`rahi_store::Lease::release`), so a tight release-then-acquire on
  one key races into its queue path, where its client reaches an
  `unreachable!()` and takes the node's locking subsystem down. That was
  observed, not assumed, while building this spec.
  Losing the lease late in a long drain is safe here rather than merely
  tolerated, which is why the design has no cursor: a batch is defined by
  what is *left*, every statement in it is idempotent, and the tombstone a
  second holder would write is the one the first holder already wrote. What
  exclusivity buys is avoided duplicate work, not correctness.
  The chassis hazard belongs to rahi and is reported rather than worked
  around here; this spec does not fork the chassis to fix it (constitution
  VI).
- **D-8 (2026-09-17, build session).** Every write in this spec takes the
  time from its caller rather than reading a clock. The crate reads no
  clock anywhere else either, and a pass whose timestamps a test could not
  choose is a pass a test could not assert about.
- **D-1 (2026-09-03, this spec).** Erasure retains a tombstone row rather
  than deleting it outright. A hard delete breaks referential integrity
  from derived memories and from recall traces, and produces a system that
  cannot explain why a citation is missing. The tombstone carries no
  content, which is what the erasure right requires.

## Verification

```verify:cli
cargo test -p aicortex-store --locked --test lifecycle
cargo test -p aicortex-store --locked --test erasure
cargo test -p aicortex-store --locked --lib erasure::tests
```
