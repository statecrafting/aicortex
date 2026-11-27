//! Driving a run: pages, items, transactions, and the cursor (spec 030 B-2,
//! B-3, B-8, B-10).
//!
//! The unit of work is a page, and a page is one transaction: the stored
//! items, their mappings, the run's counters and the cursor that covers them
//! commit together, so an interrupted run resumes exactly where it stopped
//! (B-3). Every transaction opens with one guard statement, written on the
//! lease's own fence row, that aborts the whole batch unless three things
//! still hold at commit: the lease token is the newest for the key, the run's
//! row still says `running`, and the stored cursor is the one the page was
//! fetched from. The guard is a `CASE` that yields `NULL` into a `NOT NULL`
//! column, so an absent row and a changed row both abort, which a predicate
//! that merely matched no row would not.
//!
//! Two items of one page that touch the same external id, the same content, or
//! the same memory cannot share a transaction, because the second would be
//! staged against a store the first has not reached yet. The page is flushed
//! before the second is staged. The cursor advances only in the page's last
//! transaction, so a crash between the flushes replays the page, and the
//! mapping makes the replay an update and never a duplicate.

use std::collections::BTreeSet;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use aicortex_api::capture::now;
use aicortex_api::events::{ApiEvent, EventData, scope_label};
use aicortex_gate::{
    Admitted, Candidate as GateCandidate, DigestRef, KIND_QUARANTINE, KIND_REFUSE, LedgerEntry,
    Reason, Verdict, ledger_entry,
};
use aicortex_store::{
    DIGEST_ALGORITHM, DecisionKey, DecisionKeyRepo, DecisionRefRepo, Lifecycle, MemoryRepo,
    RefKind, ScopeId, append_decision, fingerprint,
};
use aicortex_types::{MemoryId, Scope, Status};
use rahi_ledger::{Decision, DecisionId, DecisionKind, Outcome};
use rahi_store::{Envelope, Lease, Statement, TxnBuilder, Value};
use rahi_types::{Error, Revision};
use serde::Deserialize;

use crate::adapter::{AdapterError, MAX_PAGE_ITEMS, Page, RawItem};
use crate::cursor::{Cursor, CursorRepo, seconds};
use crate::mapping::{ItemRepo, ItemRow, digest, memory_parts};
use crate::run::{RunCounts, RunFailure, RunReport, RunState, Runner, Runtime, Started};

/// The most consecutive pages without an item a run accepts: an adapter that
/// keeps returning empty pages with new cursors would otherwise never end.
const MAX_EMPTY_PAGES: u32 = 16;

/// The longest the runner waits before it looks at the cancel flag again.
const CANCEL_SLICE: Duration = Duration::from_millis(250);

/// The longest a retry waits, however many times the page has failed.
const MAX_BACKOFF: Duration = Duration::from_secs(5);

/// How a run ended, when it ended on its own terms.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ending {
    Completed,
    Exhausted,
}

/// Why a page loop stopped before its ending.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Interrupt {
    Cancelled,
    Failed(RunFailure),
}

/// What the items of a run have in common with the store, which is all a dry
/// run remembers of its own earlier items.
#[derive(Debug, Default)]
struct Overlay {
    external_ids: BTreeSet<String>,
    fingerprints: BTreeSet<String>,
}

/// One transaction being built: the guard first, then what the items stage.
struct Batch {
    txn: TxnBuilder,
    counts: RunCounts,
    external_ids: BTreeSet<String>,
    fingerprints: BTreeSet<String>,
    memories: BTreeSet<MemoryId>,
    decisions: Vec<PendingDecision>,
}

impl Batch {
    fn new(guard: Statement) -> Self {
        let mut txn = TxnBuilder::new();
        txn.push(guard);
        Self {
            txn,
            counts: RunCounts::default(),
            external_ids: BTreeSet::new(),
            fingerprints: BTreeSet::new(),
            memories: BTreeSet::new(),
            decisions: Vec::new(),
        }
    }

    /// Whether staging this item beside what the batch holds would stage it
    /// against a store the batch has not reached.
    fn overlaps(&self, external_id: &str, fingerprint: &str, reads: &Reads) -> bool {
        self.external_ids.contains(external_id)
            || self.fingerprints.contains(fingerprint)
            || reads
                .holder
                .iter()
                .chain(reads.existing.iter().map(|row| &row.memory))
                .any(|memory| self.memories.contains(memory))
    }
}

/// A Decision to append once the transaction that implies it has committed
/// (013 B-9).
struct PendingDecision {
    entry: LedgerEntry,
    id: String,
}

/// What the store already holds about one item.
struct Reads {
    holder: Option<MemoryId>,
    existing: Option<ItemRow>,
}

/// One item, mapped and judged.
struct Prepared {
    digest: String,
    candidate: GateCandidate,
    verdict: Verdict,
}

/// What the gate said, read once and exhaustively: the memory it would store,
/// and the reason it held that memory back, or the reason it refused.
enum Judged<'v> {
    Stored {
        admitted: &'v Admitted,
        held_back: Option<&'v Reason>,
    },
    Refused(&'v Reason),
}

fn judge(verdict: &Verdict) -> Judged<'_> {
    match verdict {
        Verdict::Admit(admitted) => Judged::Stored {
            admitted,
            held_back: None,
        },
        Verdict::Quarantine(admitted, reason) => Judged::Stored {
            admitted,
            held_back: Some(reason),
        },
        Verdict::Refuse(reason) => Judged::Refused(reason),
    }
}

/// What an item turned out to be, for the counts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Class {
    Admitted,
    Updated,
    Merged,
    Quarantined,
    Refused,
}

impl Class {
    /// The one place an item's count is decided, for a real run and a dry
    /// run alike, which is what makes the two agree (FR-003).
    const fn of(stores: bool, quarantine: bool, mapped: bool, held: bool) -> Self {
        if !stores {
            Self::Refused
        } else if mapped {
            Self::Updated
        } else if held {
            Self::Merged
        } else if quarantine {
            Self::Quarantined
        } else {
            Self::Admitted
        }
    }

    const fn count(self, counts: &mut RunCounts) {
        counts.seen = counts.seen.saturating_add(1);
        let slot = match self {
            Self::Admitted => &mut counts.admitted,
            Self::Updated => &mut counts.updated,
            Self::Merged => &mut counts.merged,
            Self::Quarantined => &mut counts.quarantined,
            Self::Refused => &mut counts.refused,
        };
        *slot = slot.saturating_add(1);
    }
}

#[derive(Debug, Deserialize)]
struct StateRow {
    state: String,
    cancel_requested: i64,
}

struct Drive<'a> {
    runner: &'a Runner,
    runtime: &'a Runtime,
    started: &'a Started,
    scope: &'a Scope,
    scope_id: ScopeId,
    counts: RunCounts,
    cursor: Option<Cursor>,
    overlay: Overlay,
    began: Instant,
    /// How many items of the page in hand are already committed.
    progress: usize,
    empty_pages: u32,
    ledger_failed: bool,
}

impl Runner {
    /// Drive an accepted run to its end and report it (B-2).
    ///
    /// Never an error: a run that stops, whatever the reason, is a report
    /// whose state and code say why, and whose cursor is where the last
    /// committed page left it.
    pub async fn drive(&self, runtime: &Runtime, started: Started) -> RunReport {
        let mut drive = Drive {
            runner: self,
            runtime,
            started: &started,
            scope: &started.request.scope,
            scope_id: ScopeId::of(&started.request.scope),
            counts: RunCounts::default(),
            cursor: started.resume.clone(),
            overlay: Overlay::default(),
            began: Instant::now(),
            progress: 0,
            empty_pages: 0,
            ledger_failed: false,
        };
        let ended = drive.pages().await;
        let (state, failure) = match ended {
            Ok(Ending::Completed) => (RunState::Completed, None),
            Ok(Ending::Exhausted) => (RunState::Exhausted, None),
            Err(Interrupt::Cancelled) => (RunState::Cancelled, None),
            Err(Interrupt::Failed(code)) => (RunState::Failed, Some(code)),
        };
        let report = drive.finish(state, failure).await;
        drive.publish(&report);
        report
    }
}

impl Drive<'_> {
    const fn dry(&self) -> bool {
        self.started.request.dry_run
    }

    async fn pages(&mut self) -> Result<Ending, Interrupt> {
        let limits = self.started.request.limits;
        loop {
            if self.stop_requested().await? {
                return Err(Interrupt::Cancelled);
            }
            let remaining = limits.max_items.saturating_sub(self.counts.seen);
            if remaining == 0 {
                return Ok(Ending::Exhausted);
            }
            self.pace().await?;
            let limit = usize::try_from(remaining)
                .unwrap_or(MAX_PAGE_ITEMS)
                .min(MAX_PAGE_ITEMS);
            let page = self.fetch(limit).await?;
            self.accept(&page, limit)?;
            let last = page.next.is_none();
            self.page(&page).await?;
            if last {
                return Ok(Ending::Completed);
            }
        }
    }

    /// Whether a person asked the run to stop, and whether it is still ours.
    async fn stop_requested(&self) -> Result<bool, Interrupt> {
        if self.dry() {
            return Ok(self.started.cancel.load(Ordering::SeqCst));
        }
        let rows: Vec<StateRow> = self
            .runtime
            .store
            .query_consistent(
                "SELECT state, cancel_requested FROM ingest_run
                 WHERE run_id = $1 AND scope_id = $2",
                vec![
                    Value::from(self.started.run.as_str()),
                    Value::from(&self.scope_id),
                ],
            )
            .await
            .map_err(|_| Interrupt::Failed(RunFailure::StoreFailed))?;
        let Some(row) = rows.first() else {
            return Err(Interrupt::Failed(RunFailure::Superseded));
        };
        match RunState::parse(&row.state) {
            Some(RunState::Running) => Ok(row.cancel_requested == 1),
            Some(
                RunState::Completed | RunState::Exhausted | RunState::Cancelled | RunState::Failed,
            ) => Err(Interrupt::Failed(RunFailure::Superseded)),
            None => Err(Interrupt::Failed(RunFailure::StoreFailed)),
        }
    }

    /// Wait until the rate the run declared allows another page (B-8).
    async fn pace(&self) -> Result<(), Interrupt> {
        let rate = u64::from(self.started.request.limits.rate_per_second);
        let due = self.counts.seen.saturating_mul(1_000) / rate;
        loop {
            let elapsed = u64::try_from(self.began.elapsed().as_millis()).unwrap_or(u64::MAX);
            if elapsed >= due {
                return Ok(());
            }
            let wait = Duration::from_millis(due - elapsed).min(CANCEL_SLICE);
            tokio::time::sleep(wait).await;
            if self.stop_requested().await? {
                return Err(Interrupt::Cancelled);
            }
        }
    }

    /// Wait before retry number `attempt`, looking for a cancel as it goes.
    async fn back_off(&self, attempt: u32) -> Result<(), Interrupt> {
        let doublings = attempt.saturating_sub(1).min(10);
        let wait = self
            .started
            .request
            .limits
            .backoff
            .saturating_mul(1_u32 << doublings)
            .min(MAX_BACKOFF);
        let mut slept = Duration::ZERO;
        // The run is looked at before every retry, however short the wait: a
        // run that was taken over or cancelled must not spend its retries.
        loop {
            if self.stop_requested().await? {
                return Err(Interrupt::Cancelled);
            }
            if slept >= wait {
                return Ok(());
            }
            let slice = (wait - slept).min(CANCEL_SLICE);
            tokio::time::sleep(slice).await;
            slept += slice;
        }
    }

    /// Fetch the next page, retrying what is worth retrying (B-3).
    async fn fetch(&mut self, limit: usize) -> Result<Page, Interrupt> {
        let context = self
            .runner
            .context(&self.started.request, &self.started.source, limit);
        let adapter = self.started.source.adapter();
        let mut attempt = 0_u32;
        loop {
            match adapter.page(&context, self.cursor.as_ref()).await {
                Ok(page) => return Ok(page),
                Err(AdapterError::Fatal(_)) => {
                    return Err(Interrupt::Failed(RunFailure::AdapterFailed));
                }
                Err(AdapterError::Transient(_)) => {
                    attempt += 1;
                    if attempt > self.started.request.limits.retries {
                        return Err(Interrupt::Failed(RunFailure::RetriesExhausted));
                    }
                    self.back_off(attempt).await?;
                }
            }
        }
    }

    /// Hold a page to what the runner asked for: it is the adapter's output
    /// and nothing about it is trusted.
    fn accept(&mut self, page: &Page, limit: usize) -> Result<(), Interrupt> {
        let bad = Interrupt::Failed(RunFailure::BadPage);
        if page.items.len() > limit || page.items.iter().any(|item| item.validate().is_err()) {
            return Err(bad);
        }
        if page.next.is_some() && page.next == self.cursor {
            return Err(bad);
        }
        if page.items.is_empty() {
            self.empty_pages += 1;
            if self.empty_pages > MAX_EMPTY_PAGES {
                return Err(bad);
            }
        } else {
            self.empty_pages = 0;
        }
        Ok(())
    }

    async fn page(&mut self, page: &Page) -> Result<(), Interrupt> {
        if self.dry() {
            return self.page_dry(page).await;
        }
        self.page_real(page).await?;
        // The items committed; a Decision that could not be appended stops the
        // run here, with the cursor where the commit left it, so the next run
        // does not replay the page (030 D-6).
        if self.ledger_failed {
            return Err(Interrupt::Failed(RunFailure::LedgerUnavailable));
        }
        Ok(())
    }

    // ---- the dry run: the same judgement, and nothing written (B-10) ----

    async fn page_dry(&mut self, page: &Page) -> Result<(), Interrupt> {
        let failed = |_| Interrupt::Failed(RunFailure::StoreFailed);
        for item in &page.items {
            let Some(prepared) = self.prepare(item).map_err(failed)? else {
                self.counts.seen += 1;
                self.counts.unmapped += 1;
                continue;
            };
            let Judged::Stored {
                admitted,
                held_back,
            } = judge(&prepared.verdict)
            else {
                Class::Refused.count(&mut self.counts);
                continue;
            };
            let fp = fingerprint::of_memory(admitted.memory());
            let mapped = self.overlay.external_ids.contains(&item.external_id)
                || ItemRepo::lookup(
                    &self.runtime.store,
                    self.scope,
                    &self.started.request.source,
                    &item.external_id,
                )
                .await
                .map_err(failed)?
                .is_some();
            let held = self.overlay.fingerprints.contains(&fp)
                || MemoryRepo::new()
                    .fingerprint_holder(&self.runtime.store, self.scope, &fp)
                    .await
                    .map_err(failed)?
                    .is_some();
            Class::of(true, held_back.is_some(), mapped, held).count(&mut self.counts);
            self.overlay.external_ids.insert(item.external_id.clone());
            self.overlay.fingerprints.insert(fp);
        }
        self.cursor = page.next.clone();
        self.runner.board.update(&self.report_now());
        Ok(())
    }

    // ---- the real run: one page, one transaction (B-3) ----

    async fn page_real(&mut self, page: &Page) -> Result<(), Interrupt> {
        self.progress = 0;
        let retries = self.started.request.limits.retries;
        let mut failures = 0_u32;
        loop {
            let lease = self
                .runtime
                .store
                .lease(&lease_key(&self.scope_id, self.started))
                .await;
            let outcome = match &lease {
                Ok(lease) => self.stage_page(lease, page).await,
                Err(error) => Err(error.clone()),
            };
            if let Ok(lease) = lease {
                lease.release().await;
            }
            match outcome {
                Ok(()) => return Ok(()),
                Err(_) => {
                    failures += 1;
                    if failures > retries {
                        return Err(Interrupt::Failed(RunFailure::StoreFailed));
                    }
                    self.back_off(failures).await?;
                }
            }
        }
    }

    /// Stage the items from `progress` on and commit them with the cursor.
    async fn stage_page(&mut self, lease: &Lease, page: &Page) -> Result<(), Error> {
        let mut batch = Batch::new(self.guard(lease)?);
        let start = self.progress;
        for (index, item) in page.items.iter().enumerate().skip(start) {
            self.stage_item(lease, &mut batch, item, index).await?;
        }
        self.flush(
            lease,
            &mut batch,
            page.items.len(),
            Some(page.next.as_ref()),
        )
        .await?;
        self.publish(&self.report_now());
        Ok(())
    }

    /// The statement every transaction of the run opens with.
    fn guard(&self, lease: &Lease) -> Result<Statement, Error> {
        let token = i64::try_from(lease.token.get())
            .map_err(|_| Error::Integrity("a fencing token is beyond i64".to_owned()))?;
        Ok(Statement::with_params(
            "UPDATE lease_fence
             SET token = CASE WHEN token <= ?1
                 AND EXISTS (SELECT 1 FROM ingest_run
                             WHERE run_id = ?3 AND scope_id = ?4 AND state = 'running')
                 AND (SELECT cursor FROM ingest_cursor
                      WHERE scope_id = ?4 AND source_id = ?5) IS ?6
                 THEN token ELSE NULL END
             WHERE lease_key = ?2",
            vec![
                Value::Integer(token),
                Value::from(lease.key()),
                Value::from(self.started.run.as_str()),
                Value::from(&self.scope_id),
                Value::from(self.started.request.source.as_str()),
                Value::from(
                    self.cursor
                        .as_ref()
                        .map(|cursor| cursor.as_str().to_owned()),
                ),
            ],
        ))
    }

    /// Commit what the batch holds, and append the Decisions it implies.
    ///
    /// `through` is how many of the page's items the batch covers. With
    /// `finish` the cursor moves to the page's `next` (or is cleared, when
    /// there is none) in the same transaction.
    async fn flush(
        &mut self,
        lease: &Lease,
        batch: &mut Batch,
        through: usize,
        finish: Option<Option<&Cursor>>,
    ) -> Result<(), Error> {
        let fresh = Batch::new(self.guard(lease)?);
        let mut done = std::mem::replace(batch, fresh);
        let at = now();
        done.txn.push(Statement::with_params(
            "UPDATE ingest_run
             SET seen = seen + ?1, admitted = admitted + ?2, updated = updated + ?3,
                 merged = merged + ?4, quarantined = quarantined + ?5, refused = refused + ?6,
                 unmapped = unmapped + ?7, touched = ?8
             WHERE run_id = ?9 AND scope_id = ?10",
            vec![
                count_value(done.counts.seen),
                count_value(done.counts.admitted),
                count_value(done.counts.updated),
                count_value(done.counts.merged),
                count_value(done.counts.quarantined),
                count_value(done.counts.refused),
                count_value(done.counts.unmapped),
                Value::Integer(seconds(at)),
                Value::from(self.started.run.as_str()),
                Value::from(&self.scope_id),
            ],
        ));
        if let Some(next) = finish {
            let source = &self.started.request.source;
            match next {
                Some(cursor) => CursorRepo::stage_advance(
                    &mut done.txn,
                    self.scope,
                    source,
                    cursor,
                    self.started.run.as_str(),
                    at,
                ),
                None => CursorRepo::stage_clear(&mut done.txn, self.scope, source),
            }
        }
        self.runtime.store.txn(done.txn.into_statements()).await?;

        self.counts.add(&done.counts);
        self.progress = through;
        if let Some(next) = finish {
            self.cursor = next.cloned();
        }
        for pending in done.decisions {
            if self.append(&pending).await.is_err() {
                self.ledger_failed = true;
            }
        }
        Ok(())
    }

    async fn stage_item(
        &mut self,
        lease: &Lease,
        batch: &mut Batch,
        item: &RawItem,
        index: usize,
    ) -> Result<(), Error> {
        let Some(prepared) = self.prepare(item)? else {
            batch.counts.seen += 1;
            batch.counts.unmapped += 1;
            return Ok(());
        };
        let source = &self.started.request.source;
        let (admitted, held_back) = match judge(&prepared.verdict) {
            Judged::Stored {
                admitted,
                held_back,
            } => (admitted, held_back),
            Judged::Refused(reason) => {
                Class::Refused.count(&mut batch.counts);
                self.stage_decision(batch, &prepared, reason, KIND_REFUSE, None)?;
                return Ok(());
            }
        };

        let fp = fingerprint::of_memory(admitted.memory());
        let mut reads = self.read(item, &fp).await?;
        if batch.overlaps(&item.external_id, &fp, &reads) {
            self.flush(lease, batch, index, None).await?;
            reads = self.read(item, &fp).await?;
        }

        let store = &self.runtime.store;
        let at = admitted.memory().created;
        let work = work(self.scope, admitted.memory().id);
        let captured = Lifecycle::new()
            .capture(store, &mut batch.txn, admitted, &work)
            .await?;
        let stored = captured.id();
        let quarantine = held_back.is_some();

        // A changed item supersedes what it was imported as before, when that
        // is still the live memory (014 B-3). A quarantined replacement waits
        // for review and leaves the earlier memory as it was.
        if let Some(row) = &reads.existing
            && !quarantine
            && row.memory != stored
            && MemoryRepo::new()
                .get(store, self.scope, row.memory)
                .await?
                .is_some_and(|memory| memory.status == Status::Active)
        {
            Lifecycle::new()
                .supersede(store, &mut batch.txn, self.scope, row.memory, stored, at)
                .await?;
        }
        ItemRepo::stage_upsert(
            &mut batch.txn,
            self.scope,
            source,
            &item.external_id,
            stored,
            &prepared.digest,
            at,
        );

        if let Some(reason) = held_back {
            self.stage_decision(batch, &prepared, reason, KIND_QUARANTINE, Some(stored))?;
        }
        Class::of(
            true,
            quarantine,
            reads.existing.is_some(),
            reads.holder.is_some(),
        )
        .count(&mut batch.counts);
        batch.external_ids.insert(item.external_id.clone());
        batch.fingerprints.insert(fp);
        batch.memories.insert(stored);
        batch.memories.extend(reads.holder);
        batch.memories.extend(reads.existing.map(|row| row.memory));
        Ok(())
    }

    async fn read(&self, item: &RawItem, fp: &str) -> Result<Reads, Error> {
        Ok(Reads {
            holder: MemoryRepo::new()
                .fingerprint_holder(&self.runtime.store, self.scope, fp)
                .await?,
            existing: ItemRepo::lookup(
                &self.runtime.store,
                self.scope,
                &self.started.request.source,
                &item.external_id,
            )
            .await?,
        })
    }

    /// Map an item and have the gate judge it.
    ///
    /// `None` when the adapter cannot map it: the item is skipped and
    /// counted, and the run goes on.
    fn prepare(&self, item: &RawItem) -> Result<Option<Prepared>, Error> {
        let adapter = self.started.source.adapter();
        let Ok(mapped) = adapter.map(item) else {
            return Ok(None);
        };
        let (origin, trust) = self.runner.policy.admission_for(adapter.kind());
        let parts = memory_parts(
            self.scope,
            &self.started.request.source,
            &self.started.run,
            item,
            &mapped,
            trust,
            now(),
        )?;
        let candidate = GateCandidate::new(parts, origin);
        let verdict = self.runtime.gate.evaluate(&candidate);
        Ok(Some(Prepared {
            digest: digest(&mapped),
            candidate,
            verdict,
        }))
    }

    /// Stage what a refusal or a quarantine owes: the digest key, and for a
    /// quarantine the reference that commits with the row (024 B-3). The
    /// Decision itself is appended once the batch has committed.
    fn stage_decision(
        &self,
        batch: &mut Batch,
        prepared: &Prepared,
        reason: &Reason,
        kind: &'static str,
        stored: Option<MemoryId>,
    ) -> Result<(), Error> {
        let key = DecisionKey::mint()?;
        let digest = DigestRef {
            digest: key.digest(&self.runtime.gate.digest_material(&prepared.candidate)),
            algorithm: DIGEST_ALGORITHM.to_owned(),
            key_id: key.id().to_string(),
        };
        let created = prepared.candidate.parts.created;
        DecisionKeyRepo::stage(&mut batch.txn, self.scope, &key, stored, created);
        let entry = ledger_entry(
            kind,
            reason,
            self.scope,
            &self.started.request.principal,
            &digest,
            stored,
        );
        let id = format!("{}-{}", entry.kind, prepared.candidate.parts.id);
        if let Some(memory) = stored {
            DecisionRefRepo::stage(
                &mut batch.txn,
                self.scope,
                memory,
                &id,
                RefKind::Quarantine,
                created,
            );
        }
        batch.decisions.push(PendingDecision { entry, id });
        Ok(())
    }

    async fn append(&self, pending: &PendingDecision) -> Result<(), Error> {
        let entry = &pending.entry;
        let outcome = if entry.denied {
            Outcome::Deny
        } else {
            Outcome::Allow
        };
        append_decision(
            &self.runtime.ledger,
            Decision::new(
                DecisionId::new(&pending.id),
                DecisionKind::new(entry.kind),
                entry.actor.clone(),
                outcome,
                entry.reason.clone(),
            )
            .with_payload(entry.payload.clone()),
        )
        .await?;
        Ok(())
    }

    // ---- the end of a run ----

    fn report_now(&self) -> RunReport {
        self.started.report(RunState::Running, self.counts)
    }

    /// Record the run's ending and read back what the store says it was.
    async fn finish(&self, state: RunState, failure: Option<RunFailure>) -> RunReport {
        let at = now();
        let mut report = self.started.report(state, self.counts);
        report.failure = failure;
        report.finished = Some(at.get());
        if self.dry() {
            report.cancel_requested = state == RunState::Cancelled;
            self.runner.board.update(&report);
            return report;
        }
        let ended = self
            .runtime
            .store
            .txn(vec![Statement::with_params(
                "UPDATE ingest_run
                 SET state = ?1, error_code = ?2, finished = ?3, touched = ?3
                 WHERE run_id = ?4 AND scope_id = ?5 AND state = 'running'",
                vec![
                    Value::from(state.label()),
                    Value::from(failure.map(|code| code.label())),
                    Value::Integer(seconds(at)),
                    Value::from(self.started.run.as_str()),
                    Value::from(&self.scope_id),
                ],
            )])
            .await;
        // The row is the truth: a run that was taken over reads as the run
        // that took it, and a store that cannot be reached leaves the
        // in-memory figures, which are what the process knows.
        let stored = match ended {
            Ok(_) => {
                self.runner
                    .report(
                        &self.runtime.store,
                        self.scope,
                        &self.started.request.source,
                        &self.started.run,
                    )
                    .await
            }
            Err(error) => Err(error),
        };
        stored.ok().flatten().unwrap_or(report)
    }

    /// Say where the run is on the node's event bus (020 B-6).
    fn publish(&self, report: &RunReport) {
        self.runtime.events.publish(ApiEvent {
            owner: self.started.request.scope.owner.clone(),
            name: "ingest",
            data: EventData {
                scope: scope_label(self.scope),
                id: Some(report.run_id.clone()),
                state: report.state.label().to_owned(),
                reason_code: report.failure.map(|code| code.label().to_owned()),
            },
        });
    }
}

/// The lease the pages of one `(scope, source)` serialize on (B-2).
fn lease_key(scope: &ScopeId, started: &Started) -> String {
    format!(
        "aicortex.ingest/{}/{}",
        scope.as_str(),
        started.request.source
    )
}

/// The outbox work a capture stages (012 B-3).
fn work(scope: &Scope, id: MemoryId) -> Envelope {
    Envelope::new(
        "memory",
        Some(scope.owner.as_str().to_owned()),
        id.to_string(),
        Revision::new(1),
    )
}

fn count_value(count: u64) -> Value {
    Value::Integer(i64::try_from(count).unwrap_or(i64::MAX))
}
