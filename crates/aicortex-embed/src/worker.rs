//! Durable embedding work over Rahi's fenced processing queue.

use std::time::Duration;

use aicortex_types::{Memory, MemoryId, Status};
use rahi_store::{
    Claim, FailureDetail, ProcessingKey, ReceiptKey, RetryPolicy, Statement, StoreHandle,
    TxnBuilder, Value, Work,
};
use rahi_types::{Error, UnixSeconds};
use serde::Deserialize;

pub use aicortex_store::{EMBEDDING_NAMESPACE, EMBEDDING_PROCESSOR, stage_active_embedding};

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

/// Scope-specific embedding state used by preflight and operator metrics.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EmbeddingPreflight {
    /// Revision serving queries now, if one has been activated.
    pub active: Option<ModelRevision>,
    /// Durable work state for this scope.
    pub queue: QueueHealth,
    /// Live-memory coverage for each known revision in this scope.
    pub coverage: Vec<Coverage>,
}

impl EmbeddingPreflight {
    /// Read one internally consistent preflight shape for `scope_id`.
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
        let oldest = self
            .queue
            .oldest_pending_age_seconds
            .map_or_else(|| "none".to_owned(), |age| age.to_string());
        let coverage = self
            .coverage
            .iter()
            .map(|item| format!("{}:{}/{}", item.revision, item.embedded, item.total))
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
}

#[derive(Debug, Deserialize)]
struct MemoryRow {
    record: String,
}

#[derive(Debug, Deserialize)]
struct HealthRow {
    pending: i64,
    dead: i64,
    oldest_created: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct MemoryIdRow {
    id: String,
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
        Ok(Self {
            provider,
            model,
            chunker,
            config,
            holder,
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
        let claims = Work::next(
            store,
            EMBEDDING_NAMESPACE,
            EMBEDDING_PROCESSOR,
            &self.holder,
            self.config.hold_for,
            now,
            self.config.batch_size,
        )
        .await?;
        let mut report = WorkerReport {
            claimed: u64::try_from(claims.len()).unwrap_or(u64::MAX),
            ..WorkerReport::default()
        };
        for claim in claims {
            match self.process(store, &claim, now).await {
                Ok(()) => report.completed = report.completed.saturating_add(1),
                Err(error) => {
                    self.record_failure(store, &claim, &error, now).await?;
                    if claim.attempt >= self.config.retry.max_attempts {
                        report.dead = report.dead.saturating_add(1);
                    } else {
                        report.failed = report.failed.saturating_add(1);
                    }
                }
            }
        }
        Ok(report)
    }

    async fn process(
        &self,
        store: &StoreHandle,
        claim: &Claim,
        now: UnixSeconds,
    ) -> Result<(), Error> {
        if claim.key.revision != self.model.revision
            || claim.key.processor_revision != self.model.model_id.as_str()
        {
            return Err(Error::Conflict(format!(
                "embedding work revision {}@{} does not match worker {}@{}",
                claim.key.processor_revision,
                claim.key.revision,
                self.model.model_id,
                self.model.revision
            )));
        }
        let rows: Vec<MemoryRow> = store
            .query_consistent(
                "SELECT record FROM memory WHERE scope_id = ?1 AND id = ?2",
                vec![
                    Value::from(claim.key.receipt.tenant.as_str()),
                    Value::from(claim.key.receipt.key.as_str()),
                ],
            )
            .await?;
        let row = rows.into_iter().next().ok_or_else(|| {
            Error::NotFound(format!(
                "embedding work names missing memory {}",
                claim.key.receipt.key
            ))
        })?;
        let memory: Memory = serde_json::from_str(&row.record).map_err(|error| {
            Error::Integrity(format!(
                "memory {} cannot be decoded for embedding: {error}",
                claim.key.receipt.key
            ))
        })?;
        if memory.status == Status::Erased {
            return Err(Error::Conflict(format!(
                "memory {} was erased before embedding",
                memory.id
            )));
        }
        let chunks = self.chunker.split(&memory.body.text);
        if chunks.is_empty() {
            return Err(Error::Validation(format!(
                "memory {} has no text to embed",
                memory.id
            )));
        }
        let text = chunks
            .iter()
            .map(|chunk| chunk.text.as_str())
            .collect::<Vec<_>>();
        let vectors = self.provider.embed(&text).await?;
        validate_batch(&self.provider, chunks.len(), &vectors)?;

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
                 (scope_id, memory_id, model_revision, ordinal, byte_start, byte_end)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                vec![
                    Value::from(claim.key.receipt.tenant.as_str()),
                    Value::from(memory.id.to_string()),
                    Value::Integer(i64::from(self.model.revision)),
                    Value::Integer(i64::from(chunk.ordinal)),
                    Value::Integer(usize_to_sql(chunk.byte_start)?),
                    Value::Integer(usize_to_sql(chunk.byte_end)?),
                ],
            ));
            txn.push(Statement::with_params(
                "INSERT INTO embedding
                 (scope_id, memory_id, model_id, model_revision, chunk_ordinal, dims,
                  normalized, vector, updated)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
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
        Work::complete(&mut txn, claim, now);
        store.txn(txn.into_statements()).await?;
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
                detail: Some(truncate(error.message(), 2048)),
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
        EMBEDDING_PROCESSOR,
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
    let key = embedding_work_key(scope_id, memory_id, model)?;
    Work::stage_work(txn, &key, now);
    Ok(())
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
    let after = cursor.map_or_else(String::new, |id| id.to_string());
    let fetch = i64::from(limit).saturating_add(1);
    let rows: Result<Vec<MemoryIdRow>, Error> = store
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
        .await;
    let result = match rows {
        Ok(rows) => {
            let more = rows.len() > usize::try_from(limit).unwrap_or(usize::MAX);
            let mut txn = TxnBuilder::new();
            let mut last = None;
            let mut staged = 0_u32;
            let take = usize::try_from(limit).unwrap_or(usize::MAX);
            for row in rows.into_iter().take(take) {
                let memory_id = row.id.parse::<MemoryId>().map_err(|error| {
                    Error::Integrity(format!("stored memory id is invalid: {error}"))
                })?;
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
        Err(error) => Err(error),
    };
    lease.release().await;
    result
}

/// Read the queue values required by preflight and metrics.
///
/// # Errors
///
/// Store errors or negative/corrupt aggregate values.
pub async fn queue_health(
    store: &StoreHandle,
    scope_id: &str,
    now: UnixSeconds,
) -> Result<QueueHealth, Error> {
    let rows: Vec<HealthRow> = store
        .query(
            "SELECT
               COALESCE(SUM(CASE WHEN state IN ('pending', 'claimed', 'failed')
                            THEN 1 ELSE 0 END), 0) AS pending,
               COALESCE(SUM(CASE WHEN state = 'dead' THEN 1 ELSE 0 END), 0) AS dead,
               MIN(CASE WHEN state IN ('pending', 'claimed', 'failed') THEN created_at END)
                   AS oldest_created
             FROM rahi_processing
             WHERE tenant = ?1 AND namespace = ?2 AND processor = ?3",
            vec![
                Value::from(scope_id),
                Value::from(EMBEDDING_NAMESPACE),
                Value::from(EMBEDDING_PROCESSOR),
            ],
        )
        .await?;
    let row = rows.into_iter().next().unwrap_or(HealthRow {
        pending: 0,
        dead: 0,
        oldest_created: None,
    });
    let oldest = row
        .oldest_created
        .map(|value| {
            u64::try_from(value)
                .map(|created| now.get().saturating_sub(created))
                .map_err(|_| Error::Integrity("oldest embedding work time is negative".to_owned()))
        })
        .transpose()?;
    Ok(QueueHealth {
        pending: u64::try_from(row.pending)
            .map_err(|_| Error::Integrity("pending embedding count is negative".to_owned()))?,
        dead: u64::try_from(row.dead)
            .map_err(|_| Error::Integrity("dead embedding count is negative".to_owned()))?,
        oldest_pending_age_seconds: oldest,
    })
}

fn error_class(error: &Error) -> &'static str {
    error.kind()
}

fn truncate(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_owned();
    }
    let mut end = limit;
    while end > 0 && !text.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    text.get(..end).unwrap_or_default().to_owned()
}

fn usize_to_sql(value: usize) -> Result<i64, Error> {
    i64::try_from(value).map_err(|_| Error::Validation(format!("byte offset {value} exceeds i64")))
}
