# Rahi chassis prerequisites for aicortex spec 014

**Written 2026-09-19 by the aicortex 014 session.** Read-only with respect
to `/Users/bart/DevWork/rahi`: nothing in that repository was edited,
branched, published or polled to produce this note. It names the work that
has to happen there, and the rahi spec that owns each item, so rahi's own
agent can pick it up.

## 1. Why aicortex 014 is still in progress

Spec 014 implements lifecycle and erasure. Its remaining gaps are three
chassis properties, all recorded as dated Status entries in
`specs/014-memory-lifecycle-and-erasure/spec.md` and all reproduced by
bounded diagnostics whose logs are under `data/`:

1. **Archived retry duplication.** An erasure Decision whose delivery is
   interrupted before acknowledgement, then sealed into the archive, is
   appended a second time on retry. Both copies carry identical content
   apart from the assigned chain parent, and full-chain verification still
   passes, so a valid hash chain is not evidence that recovery was safe.
   Evidence: `data/014-archived-retry-before.log`,
   `data/014-archived-retry-diagnostics.log`.
2. **Stale lease release after TTL takeover.** Acquire a lease, wait out
   the TTL, acquire a newer lease on the same key, release the old one
   without a fenced write: the lock handler panics and a subsequent
   acquisition on an unrelated key times out. `erase_scope` releases its
   lease exactly this way. Evidence:
   `data/014-stale-lease-diagnostic.log`.
3. **Full-node restart followed by a second lease.** After an asynchronous
   release, both the rollback and the completed-operation cases retained
   `lease_fence.token = 1` while the next call waited for its second lease.
   The run was terminated, not passed. Evidence:
   `data/014-full-node-restart-blocker.log`.

None of the three can be repaired inside this repository. The chassis is
consumed, never forked (`CLAUDE.md` invariants, constitution VI), and this
workspace is registry-only with no `[patch]`, no git source and no path
dependency (spec 010 D-1, D-2).

## 2. Prerequisite status as of 2026-09-19

Re-read against the crates.io index, the remote tag list, and the sibling
checkout, not against earlier reports.

| Prerequisite | Status | Evidence |
|---|---|---|
| Published rahi release carrying lifetime identity (PR #65, spec 042) | **Not available.** Implemented on rahi `main` at `9b38b34`, published in no release. | `index.crates.io` lists `0.1.0` and nothing else for all nine rahi crates; the only tag locally and on `origin` is `v0.1.0`. rahi `docs/design/01-consumer-contract.md`: "implemented in the repository and published in no release". |
| Published dependency resolution carrying the stale-lease-release repair | **Not available.** Upstream hiqlite PR #352 is merged and unreleased; newest published hiqlite is `0.14.0` of 2026-07-06, which predates the merge. | `index.crates.io/hi/ql/hiqlite` ends at `0.14.0`. rahi `Cargo.toml` obtains it only through `[patch.crates-io]` at its workspace root (rahi 011 D-12); rahi's own contract table says a crates.io or git consumer gets "0.14.0, without the fix". Upstream tracking: sebadob/hiqlite#366. |
| Evidence for full-node restart and a subsequent lease acquisition | **Not available.** Unverified in rahi too. | rahi contract: "full-node restart followed by a second lease is unverified". Its evidence table proves "restart with the chain verified" as the end-to-end reboot on a restored volume, which is application snapshot recovery, the same class FR-007 already exercises here. |

**A note on what does not count as evidence.** rahi's workspace tests run
under its `[patch.crates-io]`. `[patch]` is root-workspace metadata: it is
carried neither by `cargo publish` nor along a dependency edge, so a green
rahi test run says nothing about what a registry consumer executes. rahi's
contract states this in the same words. Likewise, an intact hash chain
after an archived retry is compatible with the duplicate this repository
reproduced, so chain verification is not a recovery-safety proof.

**Reconciling rahi's release documentation.** Its `README.md` heading calls
0.2.0 a release candidate and its consumer contract cites a `v0.2.0` source
tree, while the same two documents say that 0.2.0 is unpublished, that the
tag link "does not assert that the tag or release exists yet", and that a
version declaration is neither publication nor adoption. The registry and
the remote tag list settle the contradiction: `0.1.0` is the only thing a
consumer can pin. The 0.2.0 candidate text also predates 042 landing on
`main`, so it describes a tree that no longer matches `main`.

## 3. The handoff: remaining work, and who owns it

### H-1. Publish a rahi release that carries spec 042

- **Owner:** existing approved spec **039** (release and out-of-tree
  packaging), `implementation: complete`. The tag-publishes-the-crates
  mechanism exists and was exercised for `v0.1.0`; this is an execution of
  039 B-2, not new specification work. No new proposal needed.
- **Work:** settle the release identity (the prepared 0.2.0 candidate
  predates 042, so the release is either a re-prepared 0.2.0 or a 0.3.0),
  update `README.md` and `docs/design/01-consumer-contract.md` so the
  candidate text matches the tree being tagged, cut the annotated tag on
  the tested `main` commit, and let the publish job run.
- **Constraint:** 039 AC-4 makes publication a human act, and a crates.io
  version can be neither unpublished nor reused. This session does not
  drive it.
- **Unblocks:** aicortex blocker 1. Once published, 014 replaces its
  resident-only receipt delivery with 042's lifetime-idempotent append and
  lookup, and converts the three `pinned_` diagnostics into positive
  recovery assertions.

### H-2. Move hiqlite to a published release containing PR #352

- **Owner:** existing approved spec **011** (store: hiqlite), decision
  **D-12**, which states the removal condition verbatim: an upstream
  registry release containing #352, after which the patch is removed, the
  dependency moves to that version, and the lease regression is re-run
  against it. No new proposal needed for the move itself.
- **Work:** it is upstream work first. Until sebadob/hiqlite publishes a
  release containing #352 (tracking issue #366), there is nothing rahi can
  publish that gives a registry consumer the fix, because `[patch]` is not
  inherited.
- **Decision needed from the maintainer**, because the two branches differ
  in kind:
  - **(a) Wait for upstream.** aicortex 014 stays in progress for an
    indefinite period outside anyone's control here.
  - **(b) Repair release safety inside rahi-store** so that a lease release
    is safe against an unfixed `0.14.0` lock handler, for example by making
    release fenced or by declining to release a lease the holder can prove
    is superseded. Spec **012** (store coordination) owns fenced leases and
    is `implementation: complete`, and its only acceptance criterion is
    "`cargo test -p rahi-store` passes", so this needs a **new rahi
    proposal** carrying the behavior and the acceptance, not a mid-build
    edit of 012. aicortex 014 D-9 records why the obvious workarounds fail:
    `rahi_store::Lease` suppresses a stale release only after `fenced_txn`
    detects supersession, that API accepts only UPDATE and DELETE against
    tables carrying a fence column, and the erasure transaction also
    contains counter upserts and tables without one. A separate fencing
    probe leaves a race before release.
- **Unblocks:** aicortex blocker 2, and the TTL-takeover rerun 014
  acceptance requires.

### H-3. Verify full-node restart followed by a second lease acquisition

- **Owner:** **no approved rahi spec carries this acceptance today.** 012
  AC-1 is a test-suite pass, and 037 (identity recovery and live proof)
  proves restart with the chain verified as a reboot on a restored volume,
  which is a different property. This one needs a **new rahi proposal**.
- **Work the proposal has to name:**
  - a validation that stops a whole node while a lease is held, restarts
    it, and asserts that the next acquisition on that key succeeds within a
    bounded time rather than waiting on a retained fence token;
  - an assertion distinguishing this from application snapshot recovery,
    since the snapshot-restore path already passes and does not cover it;
  - whatever implementation the validation turns out to require, which may
    be release-on-shutdown, TTL expiry that survives a restart, or fence
    reconciliation on boot. It is deliberately not specified here: the
    failing observation is aicortex's, the design is rahi's.
  - the ordering against H-2, since the observed symptom (a retained
    `lease_fence.token = 1` after asynchronous release) may share a cause
    with the stale-release defect and should not be validated against the
    unfixed lock handler.
- **Unblocks:** aicortex blocker 3, which is the last of the three.

## 4. What aicortex does when H-1 to H-3 land

Sequenced, and in this repository only:

1. Adopt the coherent published chassis version across all nine crates in
   `Cargo.toml`, review the dependency changes that release carries
   (manifest and schema evolution, spec 036, changes recovery compatibility
   and states upgrade requirements), and record the pin move.
2. Replace 014's resident-only receipt delivery with the chassis's
   lifetime-idempotent API, preserving stable receipt identity, the
   original authority, the actual affected-row counts, and recoverability
   of receipts written by the current implementation.
3. Replace the three `pinned_` blocker diagnostics with positive recovery
   assertions: concurrent sealing during a retry, and unavailable, corrupt
   and ambiguous history each answered as itself rather than as absence.
4. Verify TTL takeover, full-node restart, legacy-history coverage and the
   stated upgrade requirements.
5. Run `make spine`, `make ci`, `spec-spine verify 014`, and the literal
   `aicortex ledger verify` acceptance against a real erased fixture.

None of that is performed while the prerequisites are missing, and none of
it is inferred from a passing hash-chain verification.
