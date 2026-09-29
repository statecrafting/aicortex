//! Durable embedding work over Rahi's fenced processing queue.

use std::time::{Duration, Instant};

use aicortex_types::{Memory, MemoryId, Status};
use rahi_store::{
    Claim, FailureDetail, ProcessingKey, ReceiptKey, RetryPolicy, Statement, StoreHandle,
    TxnBuilder, Value, Work,
};
use rahi_types::{Error, UnixSeconds};
use serde::Deserialize;

pub use aicortex_store::{
    ActiveEmbedding, EMBEDDING_NAMESPACE, EMBEDDING_PROCESSOR, embedding_processor,
    stage_active_embedding,
};

use crate::chunk::Chunker;
use crate::provider::{EmbeddingProvider, validate_batch};
use crate::registry::{Coverage, ModelRegistry, ModelRevision, seconds_to_sql};

/// Maximum number of memories one re-embedding scheduler pass may stage.
pub const MAX_REEMBED_BATCH: u32 = 500;

/// Bounds for one worker drain.
#[derive(Clone, Copy, Debug)]
pub struct WorkerConfig {
    /// Maximum jobs claimed in one drain.
    pub batch_size: u32,
    /// How long one claim remains valid.
    pub hold_for: Duration,
    /// Exponential retry and dead-letter policy.
    pub retry: RetryPolicy,
}

impl WorkerConfig {
    /// Validate operational bounds.
    ///
    /// # Errors
    ///
    /// [`Error::Config`] for a zero batch, hold time, attempts, or backoff.
    pub fn validate(self) -> Result<Self, Error> {
        if self.batch_size == 0
            || self.hold_for.is_zero()
            || self.retry.max_attempts == 0
            || self.retry.base.is_zero()
            || self.retry.cap.is_zero()
        {
            return Err(Error::Config(
                "embedding worker bounds must all be non-zero".to_owned(),
            ));
        }
        Ok(self)
    }
}

/// What one drain observed and committed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WorkerReport {
    /// Claims reserved from the durable queue.
    pub claimed: u64,
    /// Claims whose vectors and completion committed together.
    pub completed: u64,
    /// Claims moved to a later retry.
    pub failed: u64,
    /// Claims moved to the dead letter.
    pub dead: u64,
}

/// Queue state surfaced by preflight and metrics.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct QueueHealth {
    /// Pending, claimed, or retrying jobs.
    pub pending: u64,
    /// Jobs that exhausted the configured attempt ceiling.
    pub dead: u64,
    /// Age in seconds of the oldest unfinished job.
    pub oldest_pending_age_seconds: Option<u64>,
}

/// Embedding state used by preflight and operator metrics.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EmbeddingPreflight {
    /// Revision serving queries now, if one has been activated.
    pub active: Option<ModelRevision>,
    /// Durable work state across every embedding revision and scope.
    pub queue: QueueHealth,
    /// Live-memory coverage for each known revision in this scope.
    pub coverage: Vec<Coverage>,
}

impl EmbeddingPreflight {
    /// Read the current preflight observations for `scope_id`.
    ///
    /// The active model, queue, and coverage are separate store observations,
    /// not an atomic snapshot.
    ///
    /// # Errors
    ///
    /// Store errors or corrupt persisted values.
    pub async fn read(
        store: &StoreHandle,
        scope_id: &str,
        now: UnixSeconds,
    ) -> Result<Self, Error> {
        Ok(Self {
            active: ModelRegistry::active(store).await?,
            queue: queue_health(store, scope_id, now).await?,
            coverage: ModelRegistry::coverage(store, scope_id).await?,
        })
    }

    /// Warning text when dead embedding work makes readiness degraded.
    #[must_use]
    pub const fn readiness_warning(&self) -> Option<&'static str> {
        if self.queue.dead > 0 {
            Some("dead embedding work requires operator attention")
        } else {
            None
        }
    }
}

impl std::fmt::Display for EmbeddingPreflight {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let active = self.active.as_ref().map_or_else(
            || "none".to_owned(),
            |model| format!("{}@{}", model.model_id.as_str(), model.revision),
        );
        let oldest = self.queue.oldest_pending_age_seconds.map_or_else(
            || {
                if self.queue.pending == 0 {
                    "none".to_owned()
                } else {
                    "unknown".to_owned()
                }
            },
            |age| age.to_string(),
        );
        let coverage = self
            .coverage
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(",");
        write!(
            formatter,
            "active={active} pending={} dead={} oldest_pending_seconds={oldest} coverage=[{coverage}]",
            self.queue.pending, self.queue.dead
        )?;
        if let Some(warning) = self.readiness_warning() {
            write!(formatter, " warning={warning}")?;
        }
        Ok(())
    }
}

/// Progress from one bounded re-embedding scheduler pass.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReembeddingBatch {
    /// Work identities staged in this pass.
    pub staged: u32,
    /// Last memory considered, for the next pass.
    pub cursor: Option<MemoryId>,
    /// Whether at least one further eligible memory was observed.
    pub more: bool,
}

/// A worker bound to one active model revision.
#[derive(Clone, Debug)]
pub struct EmbeddingWorker<P> {
    provider: P,
    model: ModelRevision,
    chunker: Chunker,
    config: WorkerConfig,
    holder: String,
    processor: String,
}

#[derive(Debug, Deserialize)]
struct MemoryRow {
    record: String,
}

#[derive(Debug, Deserialize)]
struct MemoryIdRow {
    id: String,
}

#[derive(Debug)]
enum ProcessError {
    Item(Error),
    Infrastructure(Error),
}

impl<P: EmbeddingProvider> EmbeddingWorker<P> {
    /// Bind one process to the provider resolved at boot.
    ///
    /// # Errors
    ///
    /// [`Error::Config`] when the worker bounds, holder, or model/provider
    /// identity disagree.
    pub fn new(
        provider: P,
        model: ModelRevision,
        chunker: Chunker,
        config: WorkerConfig,
        holder: impl Into<String>,
    ) -> Result<Self, Error> {
        let config = config.validate()?;
        let holder = holder.into();
        if holder.is_empty() {
            return Err(Error::Config(
                "embedding worker holder must not be empty".to_owned(),
            ));
        }
        if provider.id() != model.model_id
            || provider.dims() != model.dims
            || provider.normalized() != model.normalized
        {
            return Err(Error::Config(
                "embedding provider does not match the active model revision".to_owned(),
            ));
        }
        let processor = embedding_processor(model.revision);
        Ok(Self {
            provider,
            model,
            chunker,
            config,
            holder,
            processor,
        })
    }

    /// Drain one bounded batch.
    ///
    /// Each successful item writes all chunks and vectors in the same
    /// transaction as the fenced work completion. A failure records a
    /// bounded error and either retries or dead-letters under the same claim.
    ///
    /// # Errors
    ///
    /// Store errors while claiming or committing work.
    pub async fn drain(
        &self,
        store: &StoreHandle,
        now: UnixSeconds,
    ) -> Result<WorkerReport, Error> {
        let active = ModelRegistry::active(store).await?;
        let is_active_worker = active.as_ref().is_some_and(|active| {
            active.revision == self.model.revision && active.model_id == self.model.model_id
        });
        if !is_active_worker {
            return Ok(WorkerReport::default());
        }
        let mut claims = Vec::new();
        let mut remaining = self.config.batch_size;
        let counts = Work::counts(store).await?;
        for count in counts.iter().filter(|count| {
            count
                .processor
                .starts_with(&format!("{EMBEDDING_PROCESSOR}.r"))
                && count.processor != self.processor
                && count
                    .pending
                    .saturating_add(count.claimed)
                    .saturating_add(count.failed)
                    > 0
        }) {
            let mut stale = Work::next(
                store,
                EMBEDDING_NAMESPACE,
                &count.processor,
                &self.holder,
                self.config.hold_for,
                now,
                remaining,
            )
            .await?;
            remaining = remaining.saturating_sub(u32::try_from(stale.len()).unwrap_or(u32::MAX));
            claims.append(&mut stale);
            if remaining == 0 {
                break;
            }
        }
        if remaining > 0 {
            let mut current = Work::next(
                store,
                EMBEDDING_NAMESPACE,
                &self.processor,
                &self.holder,
                self.config.hold_for,
                now,
                remaining,
            )
            .await?;
            claims.append(&mut current);
        }
        let started = Instant::now();
        let mut report = WorkerReport {
            claimed: u64::try_from(claims.len()).unwrap_or(u64::MAX),
            ..WorkerReport::default()
        };
        for claim in claims {
            let item_now = elapsed_now(now, started);
            let claim = match Work::renew(store, &claim, self.config.hold_for, item_now).await {
                Ok(claim) => claim,
                Err(Error::Conflict(_)) => continue,
                Err(error) => return Err(error),
            };
            match self.process(store, &claim, now, started).await {
                Ok(()) => {
                    report.completed = report.completed.saturating_add(1);
                }
                Err(ProcessError::Item(error)) => {
                    match self
                        .record_failure(store, &claim, &error, elapsed_now(now, started))
                        .await
                    {
                        Ok(()) => {}
                        Err(Error::Conflict(_)) => continue,
                        Err(error) => return Err(error),
                    }
                    if claim.attempt >= self.config.retry.max_attempts {
                        report.dead = report.dead.saturating_add(1);
                    } else {
                        report.failed = report.failed.saturating_add(1);
                    }
                }
                Err(ProcessError::Infrastructure(Error::Conflict(_))) => continue,
                Err(ProcessError::Infrastructure(error)) => return Err(error),
            }
        }
        Ok(report)
    }

    async fn process(
        &self,
        store: &StoreHandle,
        claim: &Claim,
        origin: UnixSeconds,
        started: Instant,
    ) -> Result<(), ProcessError> {
        let now = elapsed_now(origin, started);
        if claim.key.processor != embedding_processor(claim.key.revision) {
            return Err(ProcessError::Item(Error::Conflict(format!(
                "embedding work processor {} does not match revision {}",
                claim.key.processor, claim.key.revision,
            ))));
        }
        match ModelRegistry::active(store)
            .await
            .map_err(ProcessError::Infrastructure)?
        {
            Some(active)
                if claim.key.revision == self.model.revision
                    && claim.key.processor_revision == self.model.model_id.as_str()
                    && active.revision == self.model.revision
                    && active.model_id == self.model.model_id => {}
            Some(active) => {
                self.restage_active(store, claim, &active, now).await?;
                return Ok(());
            }
            None => {
                // The pre-claim check handles the steady state. If deactivation
                // races a held claim, close that obsolete revision so it cannot
                // age into a retry or dead letter. A later activation stages its
                // own revision through the re-embedding scheduler.
                self.complete_empty(store, claim, now).await?;
                return Ok(());
            }
        }
        let rows: Vec<MemoryRow> = store
            .query_consistent(
                "SELECT record FROM memory WHERE scope_id = ?1 AND id = ?2",
                vec![
                    Value::from(claim.key.receipt.tenant.as_str()),
                    Value::from(claim.key.receipt.key.as_str()),
                ],
            )
            .await
            .map_err(ProcessError::Infrastructure)?;
        let Some(row) = rows.into_iter().next() else {
            self.complete_empty(store, claim, now).await?;
            return Ok(());
        };
        let memory: Memory = serde_json::from_str(&row.record).map_err(|error| {
            ProcessError::Item(Error::Integrity(format!(
                "memory {} cannot be decoded for embedding: {error}",
                claim.key.receipt.key
            )))
        })?;
        if memory.status == Status::Erased {
            self.complete_empty(store, claim, now).await?;
            return Ok(());
        }
        let chunks = self
            .chunker
            .split(&memory.body.text)
            .map_err(ProcessError::Item)?;
        if chunks.is_empty() {
            return Err(ProcessError::Item(Error::Validation(format!(
                "memory {} has no text to embed",
                memory.id
            ))));
        }
        let text = chunks
            .iter()
            .map(|chunk| chunk.text.as_str())
            .collect::<Vec<_>>();
        let vectors = self
            .provider
            .embed(&text)
            .await
            .map_err(ProcessError::Item)?;
        validate_batch(&self.provider, chunks.len(), &vectors).map_err(ProcessError::Item)?;
        let now = elapsed_now(origin, started);
        let claim = Work::renew(store, claim, self.config.hold_for, now)
            .await
            .map_err(ProcessError::Infrastructure)?;

        let mut txn = TxnBuilder::new();
        txn.push(Statement::with_params(
            "DELETE FROM embedding
             WHERE scope_id = ?1 AND memory_id = ?2 AND model_revision = ?3",
            vec![
                Value::from(claim.key.receipt.tenant.as_str()),
                Value::from(memory.id.to_string()),
                Value::Integer(i64::from(self.model.revision)),
            ],
        ));
        txn.push(Statement::with_params(
            "DELETE FROM chunk
             WHERE scope_id = ?1 AND memory_id = ?2 AND model_revision = ?3",
            vec![
                Value::from(claim.key.receipt.tenant.as_str()),
                Value::from(memory.id.to_string()),
                Value::Integer(i64::from(self.model.revision)),
            ],
        ));
        for (chunk, vector) in chunks.iter().zip(vectors.iter()) {
            txn.push(Statement::with_params(
                "INSERT INTO chunk
                 (chunk_id, scope_id, memory_id, model_revision, ordinal, byte_start, byte_end)
                 SELECT ?7, ?1, ?2, ?3, ?4, ?5, ?6
                 WHERE EXISTS (SELECT 1 FROM memory
                               WHERE scope_id = ?1 AND id = ?2 AND status <> 'erased')",
                vec![
                    Value::from(claim.key.receipt.tenant.as_str()),
                    Value::from(memory.id.to_string()),
                    Value::Integer(i64::from(self.model.revision)),
                    Value::Integer(i64::from(chunk.ordinal)),
                    Value::Integer(usize_to_sql(chunk.byte_start).map_err(ProcessError::Item)?),
                    Value::Integer(usize_to_sql(chunk.byte_end).map_err(ProcessError::Item)?),
                    Value::from(chunk_id(
                        claim.key.receipt.tenant.as_str(),
                        memory.id,
                        self.model.revision,
                        chunk.ordinal,
                    )),
                ],
            ));
            txn.push(Statement::with_params(
                "INSERT INTO embedding
                 (scope_id, memory_id, model_id, model_revision, chunk_ordinal, dims,
                  normalized, vector, updated)
                 SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9
                 WHERE EXISTS (SELECT 1 FROM memory
                               WHERE scope_id = ?1 AND id = ?2 AND status <> 'erased')",
                vec![
                    Value::from(claim.key.receipt.tenant.as_str()),
                    Value::from(memory.id.to_string()),
                    Value::from(self.model.model_id.as_str()),
                    Value::Integer(i64::from(self.model.revision)),
                    Value::Integer(i64::from(chunk.ordinal)),
                    Value::Integer(i64::from(vector.dims())),
                    Value::from(self.model.normalized),
                    Value::from(vector.to_le_bytes()),
                    Value::Integer(seconds_to_sql(now)),
                ],
            ));
        }
        Work::complete(&mut txn, &claim, now);
        store
            .txn(txn.into_statements())
            .await
            .map_err(ProcessError::Infrastructure)?;
        Ok(())
    }

    async fn complete_empty(
        &self,
        store: &StoreHandle,
        claim: &Claim,
        now: UnixSeconds,
    ) -> Result<(), ProcessError> {
        let mut txn = TxnBuilder::new();
        Work::complete(&mut txn, claim, now);
        store
            .txn(txn.into_statements())
            .await
            .map_err(ProcessError::Infrastructure)?;
        Ok(())
    }

    async fn restage_active(
        &self,
        store: &StoreHandle,
        claim: &Claim,
        active: &ModelRevision,
        now: UnixSeconds,
    ) -> Result<(), ProcessError> {
        let memory_id = claim.key.receipt.key.parse::<MemoryId>().map_err(|error| {
            ProcessError::Item(Error::Integrity(format!(
                "embedding work memory id {} is invalid: {error}",
                claim.key.receipt.key
            )))
        })?;
        let mut txn = TxnBuilder::new();
        stage_embedding(
            &mut txn,
            claim.key.receipt.tenant.as_str(),
            memory_id,
            active,
            now,
        )
        .map_err(ProcessError::Item)?;
        Work::complete(&mut txn, claim, now);
        store
            .txn(txn.into_statements())
            .await
            .map_err(ProcessError::Infrastructure)?;
        Ok(())
    }

    async fn record_failure(
        &self,
        store: &StoreHandle,
        claim: &Claim,
        error: &Error,
        now: UnixSeconds,
    ) -> Result<(), Error> {
        let mut txn = TxnBuilder::new();
        Work::fail(
            &mut txn,
            claim,
            &FailureDetail {
                class: error_class(error).to_owned(),
                detail: None,
            },
            &self.config.retry,
            now,
        )?;
        store.txn(txn.into_statements()).await?;
        Ok(())
    }
}

/// Build the durable identity for one memory and model revision.
///
/// # Errors
///
/// Rahi validation errors for an invalid scope, memory, or model identity.
pub fn embedding_work_key(
    scope_id: &str,
    memory_id: MemoryId,
    model: &ModelRevision,
) -> Result<ProcessingKey, Error> {
    ProcessingKey::new(
        ReceiptKey::new(scope_id, EMBEDDING_NAMESPACE, memory_id.to_string())?,
        model.revision,
        embedding_processor(model.revision),
        model.model_id.as_str(),
    )
}

/// Stage embedding work in the caller's capture transaction (B-1).
///
/// The processing identity is `(memory_id, model_revision)` and Rahi's
/// unique key makes repeated staging idempotent.
///
/// # Errors
///
/// Rahi validation errors from [`embedding_work_key`].
pub fn stage_embedding(
    txn: &mut TxnBuilder,
    scope_id: &str,
    memory_id: MemoryId,
    model: &ModelRevision,
    now: UnixSeconds,
) -> Result<(), Error> {
    if !model.active {
        return Err(Error::Validation(
            "embedding work requires an active model revision".to_owned(),
        ));
    }
    stage_active_embedding(
        txn,
        scope_id,
        memory_id,
        &ActiveEmbedding {
            model_id: model.model_id.as_str().to_owned(),
            revision: model.revision,
        },
        now,
    )
}

/// Stage one bounded, idempotent re-embedding pass for a target revision.
///
/// The scheduler holds a scope-and-revision lease while it selects live
/// memories without a vector at the target revision and commits their work
/// identities. A cursor allows callers to repeat bounded passes without an
/// unbounded scan. Rahi's processing key makes a repeated pass harmless.
/// Rahi 0.4.0 cannot submit `INSERT` statements through `fenced_txn`, and its
/// lease guard is intentionally private. The pass therefore uses the public
/// lease for serialization and relies on the processing identity for safety
/// if an expired holder overlaps its successor. Both may stage the same key,
/// but `ON CONFLICT DO NOTHING` leaves one durable work item.
///
/// # Errors
///
/// Store, lease, identifier, or configuration errors. `limit` must be in
/// `1..=MAX_REEMBED_BATCH`, and `model` must be active.
pub async fn stage_reembedding_batch(
    store: &StoreHandle,
    scope_id: &str,
    model: &ModelRevision,
    cursor: Option<MemoryId>,
    limit: u32,
    now: UnixSeconds,
) -> Result<ReembeddingBatch, Error> {
    if limit == 0 || limit > MAX_REEMBED_BATCH {
        return Err(Error::Config(format!(
            "re-embedding batch limit must be in 1..={MAX_REEMBED_BATCH}"
        )));
    }
    if !model.active {
        return Err(Error::Validation(
            "re-embedding requires an active target model revision".to_owned(),
        ));
    }
    let lease_key = format!("aicortex.embed.reembed/{scope_id}/{}", model.revision);
    let lease = store.lease(&lease_key).await?;
    let result = reembed_under_lease(store, scope_id, model, cursor, limit, now).await;
    lease.release().await;
    result
}

async fn reembed_under_lease(
    store: &StoreHandle,
    scope_id: &str,
    model: &ModelRevision,
    cursor: Option<MemoryId>,
    limit: u32,
    now: UnixSeconds,
) -> Result<ReembeddingBatch, Error> {
    let after = cursor.map_or_else(String::new, |id| id.to_string());
    let fetch = i64::from(limit).saturating_add(1);
    let rows: Vec<MemoryIdRow> = store
        .query_consistent(
            "SELECT memory.id AS id FROM memory
             WHERE memory.scope_id = ?1 AND memory.status <> 'erased'
               AND memory.id > ?2
               AND NOT EXISTS (
                 SELECT 1 FROM embedding
                 WHERE embedding.scope_id = ?1
                   AND embedding.memory_id = memory.id
                   AND embedding.model_revision = ?3
               )
             ORDER BY memory.id LIMIT ?4",
            vec![
                Value::from(scope_id),
                Value::from(after),
                Value::Integer(i64::from(model.revision)),
                Value::Integer(fetch),
            ],
        )
        .await?;
    let more = rows.len() > usize::try_from(limit).unwrap_or(usize::MAX);
    let mut txn = TxnBuilder::new();
    let mut last = None;
    let mut staged = 0_u32;
    let take = usize::try_from(limit).unwrap_or(usize::MAX);
    for row in rows.into_iter().take(take) {
        let memory_id = row
            .id
            .parse::<MemoryId>()
            .map_err(|error| Error::Integrity(format!("stored memory id is invalid: {error}")))?;
        stage_embedding(&mut txn, scope_id, memory_id, model, now)?;
        last = Some(memory_id);
        staged = staged.saturating_add(1);
    }
    if !txn.is_empty() {
        store.txn(txn.into_statements()).await?;
    }
    Ok(ReembeddingBatch {
        staged,
        cursor: last.or(cursor),
        more,
    })
}

/// Read the global queue values required by preflight and metrics.
///
/// Rahi 0.4 exposes processor-level counts but not tenant-level counts or
/// enqueue timestamps. The scope and clock remain in this API so callers do
/// not need another compatibility break when the chassis adds those reads.
///
/// # Errors
///
/// Store errors or negative/corrupt aggregate values.
pub async fn queue_health(
    store: &StoreHandle,
    _scope_id: &str,
    _now: UnixSeconds,
) -> Result<QueueHealth, Error> {
    let counts = Work::counts(store).await?;
    let mut pending = 0_u64;
    let mut dead = 0_u64;
    for count in counts.iter().filter(|count| {
        count
            .processor
            .starts_with(&format!("{EMBEDDING_PROCESSOR}.r"))
    }) {
        pending = pending
            .saturating_add(count.pending)
            .saturating_add(count.claimed)
            .saturating_add(count.failed);
        dead = dead.saturating_add(count.dead);
    }
    Ok(QueueHealth {
        pending,
        dead,
        oldest_pending_age_seconds: None,
    })
}

fn error_class(error: &Error) -> &'static str {
    error.kind()
}

fn elapsed_now(origin: UnixSeconds, started: Instant) -> UnixSeconds {
    UnixSeconds::new(origin.get().saturating_add(started.elapsed().as_secs()))
}

fn chunk_id(scope_id: &str, memory_id: MemoryId, revision: u32, ordinal: u32) -> String {
    let identity = format!("{scope_id}\u{1f}{memory_id}\u{1f}{revision}\u{1f}{ordinal}");
    identity
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn usize_to_sql(value: usize) -> Result<i64, Error> {
    i64::try_from(value).map_err(|_| Error::Validation(format!("byte offset {value} exceeds i64")))
}
