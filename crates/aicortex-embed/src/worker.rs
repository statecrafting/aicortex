//! Durable embedding work over Rahi's fenced processing queue.

use std::mem::size_of;
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

/// Maximum raw vector bytes submitted by one derivative transaction.
const MAX_DERIVATIVE_VECTOR_BYTES: usize = 1024 * 1024;

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
            || self.hold_for < Duration::from_secs(2)
            || self.retry.max_attempts == 0
            || self.retry.base.is_zero()
            || self.retry.cap.is_zero()
        {
            return Err(Error::Config(
                "embedding worker bounds must be non-zero and hold_for must be at least two seconds"
                    .to_owned(),
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
    /// Claims retained as quarantined work for later governed admission.
    pub quarantined: u64,
}

/// Queue state surfaced by preflight and metrics.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct QueueHealth {
    /// Pending, claimed, or retrying jobs.
    pub pending: u64,
    /// Failed jobs that exhausted the configured attempt ceiling.
    pub dead: u64,
    /// Quarantined jobs retained for later governed admission.
    pub quarantined: u64,
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
    /// Live memories in this scope, including when no model is registered.
    pub live_memories: u64,
}

/// Deployment-wide embedding state without scope-specific coverage.
#[derive(Clone, Copy, Debug)]
pub struct EmbeddingDeployment<'a>(&'a EmbeddingPreflight);

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
            live_memories: ModelRegistry::live_total(store, scope_id).await?,
        })
    }

    /// Warning text when dead embedding work makes readiness degraded.
    #[must_use]
    pub fn readiness_warning(&self) -> Option<&'static str> {
        if self.queue.dead > 0 {
            Some("dead embedding work requires operator attention")
        } else if self.live_memories > 0 && self.active.is_none() {
            Some("live memories have no active embedding model")
        } else if let Some(active) = &self.active
            && self
                .coverage
                .iter()
                .find(|coverage| coverage.revision == active.revision)
                .is_none_or(|coverage| coverage.embedded < self.live_memories)
        {
            Some("active embedding coverage is incomplete")
        } else {
            None
        }
    }

    /// Format deployment-wide state separately from per-scope coverage.
    #[must_use]
    pub const fn deployment(&self) -> EmbeddingDeployment<'_> {
        EmbeddingDeployment(self)
    }
}

impl std::fmt::Display for EmbeddingDeployment<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let active = self.0.active.as_ref().map_or_else(
            || "none".to_owned(),
            |model| format!("{}@{}", model.model_id.as_str(), model.revision),
        );
        let oldest = self.0.queue.oldest_pending_age_seconds.map_or_else(
            || {
                if self.0.queue.pending == 0 {
                    "none".to_owned()
                } else {
                    "unknown".to_owned()
                }
            },
            |age| age.to_string(),
        );
        write!(
            formatter,
            "active={active} pending={} dead={} quarantined={} oldest_pending_seconds={oldest}",
            self.0.queue.pending, self.0.queue.dead, self.0.queue.quarantined
        )?;
        if let Some(warning) = self.0.readiness_warning() {
            write!(formatter, " warning={warning}")?;
        }
        Ok(())
    }
}

impl std::fmt::Display for EmbeddingPreflight {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let coverage = self
            .coverage
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(",");
        write!(
            formatter,
            "{} live={} coverage=[{coverage}]",
            self.deployment(),
            self.live_memories
        )
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
    status: String,
}

#[derive(Debug, Deserialize)]
struct MemoryIdRow {
    id: String,
}

#[derive(Debug, Deserialize)]
struct QueueHealthRow {
    pending: i64,
    dead: i64,
    quarantined: i64,
    oldest_pending_created_at: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct AttemptCountRow {
    count: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ProcessOutcome {
    Completed,
    Deferred,
    Quarantined(Claim),
}

#[derive(Debug)]
enum ProcessError {
    Item { error: Error, claim: Claim },
    Infrastructure(Error),
}

impl ProcessError {
    fn item(error: Error, claim: &Claim) -> Self {
        Self::Item {
            error,
            claim: claim.clone(),
        }
    }
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
        let lease = store
            .lease(&format!("aicortex.embed.worker/{}", self.model.revision))
            .await?;
        let result = async {
            let Some(active) = ModelRegistry::active(store).await? else {
                return Ok(WorkerReport::default());
            };
            if active.revision == self.model.revision && !same_model_revision(&active, &self.model)
            {
                return Ok(WorkerReport::default());
            }
            let started = Instant::now();
            let mut report = WorkerReport::default();
            for _ in 0..self.config.batch_size {
                let item_now = elapsed_now(now, started);
                let mut claims = Work::next(
                    store,
                    EMBEDDING_NAMESPACE,
                    &self.processor,
                    &self.holder,
                    self.config.hold_for,
                    item_now,
                    1,
                )
                .await?;
                let Some(claim) = claims.pop() else {
                    break;
                };
                report.claimed = report.claimed.saturating_add(1);
                match self.process(store, &claim, now, started).await {
                    Ok(ProcessOutcome::Completed) => {
                        report.completed = report.completed.saturating_add(1);
                    }
                    Ok(ProcessOutcome::Deferred) => break,
                    Ok(ProcessOutcome::Quarantined(quarantined_claim)) => {
                        match self
                            .record_quarantine(store, &quarantined_claim, elapsed_now(now, started))
                            .await
                        {
                            Ok(()) => {}
                            Err(Error::Conflict(_)) => continue,
                            Err(error) => return Err(error),
                        }
                        report.quarantined = report.quarantined.saturating_add(1);
                    }
                    Err(ProcessError::Item { error, claim }) => {
                        let dead = match self
                            .record_failure(store, &claim, &error, elapsed_now(now, started))
                            .await
                        {
                            Ok(dead) => dead,
                            Err(Error::Conflict(_)) => continue,
                            Err(error) => return Err(error),
                        };
                        if dead {
                            report.dead = report.dead.saturating_add(1);
                        } else {
                            report.failed = report.failed.saturating_add(1);
                        }
                        break;
                    }
                    Err(ProcessError::Infrastructure(Error::Conflict(_))) => continue,
                    Err(ProcessError::Infrastructure(error)) => return Err(error),
                }
            }
            Ok(report)
        }
        .await;
        lease.release().await;
        result
    }

    async fn process(
        &self,
        store: &StoreHandle,
        claim: &Claim,
        origin: UnixSeconds,
        started: Instant,
    ) -> Result<ProcessOutcome, ProcessError> {
        let now = elapsed_now(origin, started);
        if claim.key.processor != embedding_processor(claim.key.revision) {
            return Err(ProcessError::item(
                Error::Conflict(format!(
                    "embedding work processor {} does not match revision {}",
                    claim.key.processor, claim.key.revision,
                )),
                claim,
            ));
        }
        match ModelRegistry::active(store)
            .await
            .map_err(ProcessError::Infrastructure)?
        {
            Some(active)
                if claim.key.revision == self.model.revision
                    && claim.key.processor_revision == self.model.model_id.as_str()
                    && same_model_revision(&active, &self.model) => {}
            Some(_) => {
                self.complete_empty(store, claim, now).await?;
                return Ok(ProcessOutcome::Completed);
            }
            None => {
                // The pre-claim check handles the steady state. If deactivation
                // races a held claim, immediately return the same durable key to
                // pending. Completing it would prevent a later activation of
                // this revision from ever embedding the memory.
                self.defer_inactive_model(store, claim, now).await?;
                return Ok(ProcessOutcome::Deferred);
            }
        }
        let rows: Vec<MemoryRow> = store
            .query_consistent(
                "SELECT record, status FROM memory WHERE scope_id = ?1 AND id = ?2",
                vec![
                    Value::from(claim.key.receipt.tenant.as_str()),
                    Value::from(claim.key.receipt.key.as_str()),
                ],
            )
            .await
            .map_err(ProcessError::Infrastructure)?;
        let Some(row) = rows.into_iter().next() else {
            self.complete_empty(store, claim, now).await?;
            return Ok(ProcessOutcome::Completed);
        };
        if row.status == Status::Erased.label() {
            self.complete_empty(store, claim, now).await?;
            return Ok(ProcessOutcome::Completed);
        }
        if row.status == Status::Quarantined.label() {
            return Ok(ProcessOutcome::Quarantined(claim.clone()));
        }
        let memory: Memory = serde_json::from_str(&row.record).map_err(|error| {
            ProcessError::item(
                Error::Integrity(format!(
                    "memory {} cannot be decoded for embedding: {error}",
                    claim.key.receipt.key
                )),
                claim,
            )
        })?;
        let chunks = self
            .chunker
            .split(&memory.body.text)
            .map_err(|error| ProcessError::item(error, claim))?;
        if chunks.is_empty() {
            return Err(ProcessError::item(
                Error::Validation(format!("memory {} has no text to embed", memory.id)),
                claim,
            ));
        }
        let vector_bytes = chunks
            .len()
            .checked_mul(usize::from(self.model.dims))
            .and_then(|values| values.checked_mul(size_of::<f32>()));
        if vector_bytes.is_none_or(|bytes| bytes > MAX_DERIVATIVE_VECTOR_BYTES) {
            return Err(ProcessError::item(
                Error::Validation(format!(
                    "embedding derivative payload exceeds the {MAX_DERIVATIVE_VECTOR_BYTES}-byte worker limit"
                )),
                claim,
            ));
        }
        let text = chunks
            .iter()
            .map(|chunk| chunk.text.as_str())
            .collect::<Vec<_>>();
        let claim = Work::renew(
            store,
            claim,
            self.config.hold_for,
            elapsed_now(origin, started),
        )
        .await
        .map_err(ProcessError::Infrastructure)?;
        let vectors = tokio::time::timeout(self.config.hold_for / 2, self.provider.embed(&text))
            .await
            .map_err(|_| {
                ProcessError::item(
                    Error::Upstream(format!(
                        "embedding provider exceeded its {:?} processing deadline",
                        self.config.hold_for / 2
                    )),
                    &claim,
                )
            })?
            .map_err(|error| ProcessError::item(error, &claim))?;
        validate_batch(&self.provider, chunks.len(), &vectors)
            .map_err(|error| ProcessError::item(error, &claim))?;
        let now = elapsed_now(origin, started);

        let mut txn = TxnBuilder::new();
        stage_derivative_commit_guards(
            &mut txn,
            claim.key.receipt.tenant.as_str(),
            memory.id,
            &self.model,
        );
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
                               WHERE scope_id = ?1 AND id = ?2
                                 AND status NOT IN ('erased', 'quarantined'))",
                vec![
                    Value::from(claim.key.receipt.tenant.as_str()),
                    Value::from(memory.id.to_string()),
                    Value::Integer(i64::from(self.model.revision)),
                    Value::Integer(i64::from(chunk.ordinal)),
                    Value::Integer(
                        usize_to_sql(chunk.byte_start)
                            .map_err(|error| ProcessError::item(error, &claim))?,
                    ),
                    Value::Integer(
                        usize_to_sql(chunk.byte_end)
                            .map_err(|error| ProcessError::item(error, &claim))?,
                    ),
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
                               WHERE scope_id = ?1 AND id = ?2
                                 AND status NOT IN ('erased', 'quarantined'))",
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
        match store.txn(txn.into_statements()).await {
            Ok(_) => Ok(ProcessOutcome::Completed),
            Err(error) => {
                self.reconcile_commit_failure(store, &claim, error, now)
                    .await
            }
        }
    }

    async fn reconcile_commit_failure(
        &self,
        store: &StoreHandle,
        claim: &Claim,
        error: Error,
        now: UnixSeconds,
    ) -> Result<ProcessOutcome, ProcessError> {
        let rows: Vec<MemoryRow> = store
            .query_consistent(
                "SELECT record, status FROM memory WHERE scope_id = ?1 AND id = ?2",
                vec![
                    Value::from(claim.key.receipt.tenant.as_str()),
                    Value::from(claim.key.receipt.key.as_str()),
                ],
            )
            .await
            .map_err(ProcessError::Infrastructure)?;
        match rows.first().map(|row| row.status.as_str()) {
            Some(status) if status == Status::Quarantined.label() => {
                return Ok(ProcessOutcome::Quarantined(claim.clone()));
            }
            None => {
                self.complete_empty(store, claim, now).await?;
                return Ok(ProcessOutcome::Completed);
            }
            Some(status) if status == Status::Erased.label() => {
                self.complete_empty(store, claim, now).await?;
                return Ok(ProcessOutcome::Completed);
            }
            Some(_) => {}
        }
        let active = ModelRegistry::active(store)
            .await
            .map_err(ProcessError::Infrastructure)?;
        match active.as_ref() {
            None => {
                self.defer_inactive_model(store, claim, now).await?;
                return Ok(ProcessOutcome::Deferred);
            }
            Some(model) if !same_model_revision(model, &self.model) => {
                self.complete_empty(store, claim, now).await?;
                return Ok(ProcessOutcome::Completed);
            }
            Some(_) => {}
        }
        Err(ProcessError::item(error, claim))
    }

    async fn defer_inactive_model(
        &self,
        store: &StoreHandle,
        claim: &Claim,
        now: UnixSeconds,
    ) -> Result<(), ProcessError> {
        let mut txn = TxnBuilder::new();
        let terminal = RetryPolicy {
            max_attempts: claim.attempt,
            ..self.config.retry
        };
        Work::fail(
            &mut txn,
            claim,
            &FailureDetail {
                class: "model_deactivated".to_owned(),
                detail: None,
            },
            &terminal,
            now,
        )
        .map_err(ProcessError::Infrastructure)?;
        Work::requeue(&mut txn, &claim.key, now);
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

    async fn record_failure(
        &self,
        store: &StoreHandle,
        claim: &Claim,
        error: &Error,
        now: UnixSeconds,
    ) -> Result<bool, Error> {
        let retry = self.provider_retry_policy(store, claim).await?;
        let dead = claim.attempt >= retry.max_attempts;
        let mut txn = TxnBuilder::new();
        Work::fail(
            &mut txn,
            claim,
            &FailureDetail {
                class: error_class(error).to_owned(),
                detail: None,
            },
            &retry,
            now,
        )?;
        store.txn(txn.into_statements()).await?;
        Ok(dead)
    }

    async fn provider_retry_policy(
        &self,
        store: &StoreHandle,
        claim: &Claim,
    ) -> Result<RetryPolicy, Error> {
        let rows: Vec<AttemptCountRow> = store
            .query_consistent(
                "SELECT COUNT(*) AS count FROM rahi_processing_attempt
                 WHERE key_digest = ?1 AND revision = ?2 AND processor = ?3
                   AND processor_revision = ?4
                   AND error_class IN ('model_deactivated', 'quarantined')",
                vec![
                    Value::from(claim.key.receipt.key_digest()),
                    Value::Integer(i64::from(claim.key.revision)),
                    Value::from(claim.key.processor.as_str()),
                    Value::from(claim.key.processor_revision.as_str()),
                ],
            )
            .await?;
        let administrative_attempts = rows.first().map_or(0, |row| row.count);
        let administrative_attempts = u32::try_from(administrative_attempts).map_err(|_| {
            Error::Integrity("embedding administrative attempt count is invalid".to_owned())
        })?;
        let max_attempts = self
            .config
            .retry
            .max_attempts
            .checked_add(administrative_attempts)
            .ok_or_else(|| Error::Integrity("embedding retry ceiling overflowed".to_owned()))?;
        Ok(RetryPolicy {
            max_attempts,
            ..self.config.retry
        })
    }

    async fn record_quarantine(
        &self,
        store: &StoreHandle,
        claim: &Claim,
        now: UnixSeconds,
    ) -> Result<(), Error> {
        let mut txn = TxnBuilder::new();
        let terminal = RetryPolicy {
            max_attempts: claim.attempt,
            ..self.config.retry
        };
        Work::fail(
            &mut txn,
            claim,
            &FailureDetail {
                class: "quarantined".to_owned(),
                detail: None,
            },
            &terminal,
            now,
        )?;
        store.txn(txn.into_statements()).await?;
        Ok(())
    }
}

fn stage_derivative_commit_guards(
    txn: &mut TxnBuilder,
    scope_id: &str,
    memory_id: MemoryId,
    model: &ModelRevision,
) {
    stage_live_memory_guard(txn, scope_id, memory_id);
    txn.push(Statement::with_params(
        "INSERT INTO embedding_model (revision)
         SELECT ?1
         WHERE NOT EXISTS (
             SELECT 1 FROM embedding_model
             WHERE revision = ?1 AND model_id = ?2 AND dims = ?3
               AND normalized = ?4 AND active = 1
         )",
        vec![
            Value::Integer(i64::from(model.revision)),
            Value::from(model.model_id.as_str()),
            Value::Integer(i64::from(model.dims)),
            Value::from(model.normalized),
        ],
    ));
}

fn stage_live_memory_guard(txn: &mut TxnBuilder, scope_id: &str, memory_id: MemoryId) {
    txn.push(Statement::with_params(
        "INSERT INTO chunk (chunk_id)
         SELECT NULL
         WHERE NOT EXISTS (
             SELECT 1 FROM memory
             WHERE scope_id = ?1 AND id = ?2
               AND status NOT IN ('erased', 'quarantined')
         )",
        vec![Value::from(scope_id), Value::from(memory_id.to_string())],
    ));
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

/// Stage embedding work only while the selected memory remains live.
///
/// Re-embedding selects candidates before it opens the staging transaction.
/// This guard rechecks the selection inside that transaction, so erasure or
/// quarantine committed in between aborts the complete batch. If the memory
/// is no longer live, the guard deliberately violates `chunk.chunk_id`'s
/// `NOT NULL` constraint without opening an insert path into `memory`.
///
/// # Errors
///
/// Rahi validation errors from [`stage_embedding`]. The submitted transaction
/// also fails when the memory is absent, erased, or quarantined at commit.
pub fn stage_live_embedding(
    txn: &mut TxnBuilder,
    scope_id: &str,
    memory_id: MemoryId,
    model: &ModelRevision,
    now: UnixSeconds,
) -> Result<(), Error> {
    stage_live_memory_guard(txn, scope_id, memory_id);
    stage_embedding(txn, scope_id, memory_id, model, now)
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
             WHERE memory.scope_id = ?1
               AND memory.status NOT IN ('erased', 'quarantined')
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
    let mut selected = 0_usize;
    let take = usize::try_from(limit).unwrap_or(usize::MAX);
    for row in rows.into_iter().take(take) {
        let memory_id = row
            .id
            .parse::<MemoryId>()
            .map_err(|error| Error::Integrity(format!("stored memory id is invalid: {error}")))?;
        stage_live_embedding(&mut txn, scope_id, memory_id, model, now)?;
        last = Some(memory_id);
        selected = selected.saturating_add(1);
    }
    let staged = if txn.is_empty() {
        0
    } else {
        let results = store.txn(txn.into_statements()).await?;
        let expected = selected.saturating_mul(3);
        if results.len() != expected {
            return Err(Error::Integrity(format!(
                "embedding staging returned {} results for {selected} memories",
                results.len()
            )));
        }
        let mut staged = 0_u32;
        for result in results.iter().skip(2).step_by(3) {
            match result.rows_affected {
                0 => {}
                1 => staged = staged.saturating_add(1),
                count => {
                    return Err(Error::Integrity(format!(
                        "embedding work staging affected {count} rows"
                    )));
                }
            }
        }
        staged
    };
    Ok(ReembeddingBatch {
        staged,
        cursor: last.or(cursor),
        more,
    })
}

/// Read the global queue values required by preflight and metrics.
///
/// Rahi 0.4 exposes deployment-level processing rows but not a tenant-level
/// queue aggregate. The scope remains in this API so callers do not need
/// another compatibility break when the chassis adds that read.
/// Counts deliberately include every model revision: activating a replacement
/// must not hide unfinished work in the prior revision's durable partition.
/// The prior revision's worker drains that work to a terminal no-op; until it
/// does, preflight continues to report the pending work as an operator-visible
/// defect. Quarantined work is retained in Rahi's dead-letter state for later
/// governed admission, but is reported separately from failed work so a normal
/// gate outcome does not degrade readiness.
///
/// # Errors
///
/// Store errors or negative/corrupt aggregate values.
pub async fn queue_health(
    store: &StoreHandle,
    _scope_id: &str,
    now: UnixSeconds,
) -> Result<QueueHealth, Error> {
    let rows: Vec<QueueHealthRow> = store
        .query_consistent(
            "SELECT
               COALESCE(SUM(CASE
                 WHEN processing.state IN ('pending', 'claimed', 'failed') THEN 1
                 ELSE 0
               END), 0) AS pending,
               COALESCE(SUM(CASE
                 WHEN processing.state = 'dead'
                  AND NOT (
                    COALESCE(attempt.error_class, '') = 'quarantined'
                    AND COALESCE(memory.status, '') = 'quarantined'
                  ) THEN 1
                 ELSE 0
               END), 0) AS dead,
               COALESCE(SUM(CASE
                 WHEN processing.state = 'dead'
                  AND attempt.error_class = 'quarantined'
                  AND memory.status = 'quarantined' THEN 1
                 ELSE 0
               END), 0) AS quarantined,
               MIN(CASE
                 WHEN processing.state IN ('pending', 'claimed', 'failed')
                 THEN processing.created_at
               END) AS oldest_pending_created_at
             FROM rahi_processing AS processing
             LEFT JOIN rahi_processing_attempt AS attempt
               ON attempt.key_digest = processing.key_digest
              AND attempt.revision = processing.revision
              AND attempt.processor = processing.processor
              AND attempt.processor_revision = processing.processor_revision
              AND attempt.attempt = processing.attempt
              AND attempt.outcome = 'dead'
             LEFT JOIN memory
               ON memory.scope_id = processing.tenant
              AND memory.id = processing.key
             WHERE processing.state IN ('pending', 'claimed', 'failed', 'dead')
               AND processing.namespace = ?1
               AND processing.processor GLOB ?2
               AND substr(processing.processor, ?3) NOT GLOB '*[^0-9]*'",
            vec![
                Value::from(EMBEDDING_NAMESPACE),
                Value::from(format!("{EMBEDDING_PROCESSOR}.r[0-9]*")),
                Value::Integer(
                    i64::try_from(EMBEDDING_PROCESSOR.len() + 3)
                        .map_err(|_| Error::Integrity("processor prefix is too long".to_owned()))?,
                ),
            ],
        )
        .await?;
    let row = rows.first().ok_or_else(|| {
        Error::Integrity("embedding queue health query returned no aggregate row".to_owned())
    })?;
    let pending = u64::try_from(row.pending)
        .map_err(|_| Error::Integrity("pending embedding work count is negative".to_owned()))?;
    let dead = u64::try_from(row.dead)
        .map_err(|_| Error::Integrity("dead embedding work count is negative".to_owned()))?;
    let quarantined = u64::try_from(row.quarantined)
        .map_err(|_| Error::Integrity("quarantined embedding work count is negative".to_owned()))?;
    let oldest_pending_age_seconds = row
        .oldest_pending_created_at
        .map(u64::try_from)
        .transpose()
        .map_err(|_| Error::Integrity("embedding work creation time is negative".to_owned()))?
        .map(|created_at| now.get().saturating_sub(created_at));
    Ok(QueueHealth {
        pending,
        dead,
        quarantined,
        oldest_pending_age_seconds,
    })
}

fn error_class(error: &Error) -> &'static str {
    error.kind()
}

fn elapsed_now(origin: UnixSeconds, started: Instant) -> UnixSeconds {
    UnixSeconds::new(origin.get().saturating_add(started.elapsed().as_secs()))
}

fn same_model_revision(left: &ModelRevision, right: &ModelRevision) -> bool {
    left.model_id == right.model_id
        && left.revision == right.revision
        && left.dims == right.dims
        && left.normalized == right.normalized
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
