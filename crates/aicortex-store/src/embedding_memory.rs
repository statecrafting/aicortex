//! Scoped memory access needed by the embedding pipeline (spec 015).
//!
//! SQL naming the `memory` table stays in the storage crate so the scope
//! predicate ratchet of spec 012 covers every such statement. The embedding
//! crate receives decoded records, bounded identifiers, and aggregates only.

use aicortex_types::{Memory, MemoryId};
use rahi_store::{ProcessingKey, ReceiptKey, Statement, StoreHandle, TxnBuilder, Value, Work};
use rahi_types::{Error, UnixSeconds};
use serde::Deserialize;

/// Rahi processing namespace holding embedding jobs.
pub const EMBEDDING_NAMESPACE: &str = "aicortex.memory";

/// Rahi processor name holding embedding jobs.
pub const EMBEDDING_PROCESSOR: &str = "embed";

/// The model identity needed to stage embedding work without depending on
/// the embedding crate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActiveEmbedding {
    /// Configured provider model identity.
    pub model_id: String,
    /// Monotonic model configuration revision.
    pub revision: u32,
}

/// One memory as observed by a worker through the leader.
#[derive(Clone, Debug, PartialEq)]
pub struct EmbeddingMemory {
    /// The durable memory record, decoded only while the row is live.
    pub memory: Option<Memory>,
    /// The indexed status column used by worker lifecycle decisions.
    pub status: String,
}

/// Queue aggregates whose classification depends on memory lifecycle state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EmbeddingQueueCounts {
    /// Pending, claimed, or retrying work.
    pub pending: i64,
    /// Terminal work not currently held for quarantine review.
    pub dead: i64,
    /// Terminal work held for quarantine review.
    pub quarantined: i64,
    /// Creation time of the oldest unfinished item.
    pub oldest_pending_created_at: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct MemoryRow {
    record: String,
    status: String,
}

#[derive(Debug, Deserialize)]
struct IdRow {
    id: String,
}

#[derive(Debug, Deserialize)]
struct CountRow {
    count: i64,
}

#[derive(Debug, Deserialize)]
struct CoverageRow {
    revision: i64,
    embedded: i64,
}

#[derive(Debug, Deserialize)]
struct QueueRow {
    pending: i64,
    dead: i64,
    quarantined: i64,
    oldest_pending_created_at: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct ActiveEmbeddingRow {
    model_id: String,
    revision: i64,
}

/// Rahi processor partition for one model revision.
#[must_use]
pub fn embedding_processor(revision: u32) -> String {
    format!("{EMBEDDING_PROCESSOR}.r{revision}")
}

/// Read the active embedding model through the leader.
///
/// # Errors
///
/// Store errors, a missing active model, or an invalid stored revision.
pub async fn active_embedding(store: &StoreHandle) -> Result<ActiveEmbedding, Error> {
    EmbeddingTarget::observe(store)
        .await?
        .active
        .ok_or_else(|| {
            Error::Config("no active embedding model; activate one before capture".to_owned())
        })
}

/// The embedding model a memory write stages its work for (B-1).
///
/// Every insert into `memory` takes one of these, so no call site can write
/// a memory without deciding what its embedding work is. The value is the
/// caller's observation of the registry, and the statements it stages turn
/// that observation into a commit-time condition: with a model active, the
/// write stages that revision's job and aborts if activation moved; with no
/// model active, the write stages no job and aborts if one became active.
/// A memory therefore never commits next to an active model without its
/// job. Memories written while no model is active are covered by the
/// re-embedding pass that follows activation (B-9).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EmbeddingTarget {
    active: Option<ActiveEmbedding>,
}

impl EmbeddingTarget {
    /// Read the registry through the leader.
    ///
    /// # Errors
    ///
    /// Store errors, or an invalid stored revision.
    pub async fn observe(store: &StoreHandle) -> Result<Self, Error> {
        let rows: Vec<ActiveEmbeddingRow> = store
            .query_consistent(
                "SELECT model_id, revision FROM embedding_model WHERE active = 1",
                vec![],
            )
            .await?;
        let active = rows
            .into_iter()
            .next()
            .map(|row| {
                Ok::<_, Error>(ActiveEmbedding {
                    model_id: row.model_id,
                    revision: u32::try_from(row.revision).map_err(|_| {
                        Error::Integrity("active model revision is outside u32".to_owned())
                    })?,
                })
            })
            .transpose()?;
        Ok(Self { active })
    }

    /// A target that expects no model to be active at commit.
    #[must_use]
    pub const fn none() -> Self {
        Self { active: None }
    }

    /// A target that expects `model` to be active at commit.
    #[must_use]
    pub const fn active(model: ActiveEmbedding) -> Self {
        Self {
            active: Some(model),
        }
    }

    /// The model this target stages work for, if any.
    #[must_use]
    pub const fn model(&self) -> Option<&ActiveEmbedding> {
        self.active.as_ref()
    }

    /// Stage the memory's embedding work, or the guard that no model is
    /// active, into the caller's transaction.
    ///
    /// # Errors
    ///
    /// Rahi validation errors for an invalid scope or memory identity.
    pub fn stage(
        &self,
        txn: &mut TxnBuilder,
        scope_id: &str,
        memory_id: MemoryId,
        now: UnixSeconds,
    ) -> Result<(), Error> {
        match &self.active {
            Some(model) => stage_active_embedding(txn, scope_id, memory_id, model, now),
            None => {
                // Abort the whole write if a model became active after the
                // observation: the SELECT emits a row only then, and that
                // row's NULL model id cannot be inserted.
                txn.push(Statement::new(
                    "INSERT INTO embedding_model
                         (model_id, revision, dims, normalized, first_seen, active)
                     SELECT NULL, 0, 0, 0, 0, 0
                     WHERE EXISTS (SELECT 1 FROM embedding_model WHERE active = 1)",
                ));
                Ok(())
            }
        }
    }
}

/// Stage embedding work for the model observed active by the caller.
///
/// The caller reads the active model through the leader before building its
/// transaction. The guard statement makes an activation change before commit
/// abort the complete capture instead of staging work under a stale model
/// identity.
///
/// # Errors
///
/// Rahi validation errors for an invalid scope or memory identity.
pub fn stage_active_embedding(
    txn: &mut TxnBuilder,
    scope_id: &str,
    memory_id: MemoryId,
    model: &ActiveEmbedding,
    now: UnixSeconds,
) -> Result<(), Error> {
    let receipt = ReceiptKey::new(scope_id, EMBEDDING_NAMESPACE, memory_id.to_string())?;
    let key = ProcessingKey::new(
        receipt,
        model.revision,
        embedding_processor(model.revision),
        &model.model_id,
    )?;
    // Abort the whole capture if activation changed after the leader read.
    // The SELECT emits no row when the identity is still active. Otherwise
    // it attempts an impossible NULL model id, including when the revision
    // row is absent, so SQLite rolls the complete transaction back.
    txn.push(Statement::with_params(
        "INSERT INTO embedding_model
             (model_id, revision, dims, normalized, first_seen, active)
         SELECT NULL, ?1, 0, 0, 0, 0
         WHERE NOT EXISTS (
             SELECT 1 FROM embedding_model
             WHERE revision = ?1 AND model_id = ?2 AND active = 1
         )",
        vec![
            Value::Integer(i64::from(model.revision)),
            Value::from(model.model_id.as_str()),
        ],
    ));
    Work::stage_work(txn, &key, now);
    Ok(())
}

/// Read one worker input through the leader.
///
/// # Errors
///
/// Store errors or an invalid stored record.
pub async fn embedding_memory(
    store: &StoreHandle,
    scope_id: &str,
    memory_id: MemoryId,
) -> Result<Option<EmbeddingMemory>, Error> {
    let rows: Vec<MemoryRow> = store
        .query_consistent(
            "SELECT record, status FROM memory WHERE scope_id = ?1 AND id = ?2",
            vec![Value::from(scope_id), Value::from(memory_id.to_string())],
        )
        .await?;
    rows.into_iter()
        .next()
        .map(|row| {
            let memory = if matches!(row.status.as_str(), "erased" | "quarantined") {
                None
            } else {
                Some(serde_json::from_str(&row.record).map_err(|error| {
                    Error::Integrity(format!(
                        "memory {memory_id} cannot be decoded for embedding: {error}"
                    ))
                })?)
            };
            Ok(EmbeddingMemory {
                memory,
                status: row.status,
            })
        })
        .transpose()
}

/// Abort a caller's transaction unless the scoped memory remains live.
pub fn stage_live_memory_guard(txn: &mut TxnBuilder, scope_id: &str, memory_id: MemoryId) {
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

/// Select a bounded page of live memories lacking one model revision.
///
/// # Errors
///
/// Store errors or an invalid stored memory identifier.
pub async fn memories_missing_embedding(
    store: &StoreHandle,
    scope_id: &str,
    revision: u32,
    after: &str,
    limit: i64,
) -> Result<Vec<MemoryId>, Error> {
    let rows: Vec<IdRow> = store
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
                Value::Integer(i64::from(revision)),
                Value::Integer(limit),
            ],
        )
        .await?;
    rows.into_iter()
        .map(|row| {
            row.id
                .parse::<MemoryId>()
                .map_err(|error| Error::Integrity(format!("stored memory id is invalid: {error}")))
        })
        .collect()
}

/// Count live memories from the maintained scope counters.
///
/// # Errors
///
/// Store errors, a missing aggregate row, or a negative counter.
pub async fn live_memory_total(store: &StoreHandle, scope_id: &str) -> Result<u64, Error> {
    let rows: Vec<CountRow> = store
        .query(
            "SELECT COALESCE(SUM(count), 0) AS count FROM scope_counter
             WHERE scope_id = ?1 AND status NOT IN ('erased', 'quarantined')",
            vec![Value::from(scope_id)],
        )
        .await?;
    let count = rows
        .first()
        .ok_or_else(|| Error::Integrity("live memory count returned no row".to_owned()))?
        .count;
    u64::try_from(count).map_err(|_| Error::Integrity("live memory count is negative".to_owned()))
}

/// Count live memories with embeddings, grouped by model revision.
///
/// # Errors
///
/// Store errors or corrupt revision and count values.
pub async fn embedding_coverage(
    store: &StoreHandle,
    scope_id: &str,
) -> Result<Vec<(u32, u64)>, Error> {
    let rows: Vec<CoverageRow> = store
        .query(
            "SELECT embedding.model_revision AS revision,
                    COUNT(DISTINCT memory.id) AS embedded
             FROM embedding
             JOIN memory ON memory.scope_id = embedding.scope_id
                        AND memory.id = embedding.memory_id
                        AND memory.status NOT IN ('erased', 'quarantined')
             WHERE embedding.scope_id = ?1
             GROUP BY embedding.model_revision",
            vec![Value::from(scope_id)],
        )
        .await?;
    rows.into_iter()
        .map(|row| {
            Ok((
                u32::try_from(row.revision)
                    .map_err(|_| Error::Integrity("coverage revision is outside u32".to_owned()))?,
                u64::try_from(row.embedded)
                    .map_err(|_| Error::Integrity("embedded coverage is negative".to_owned()))?,
            ))
        })
        .collect()
}

/// Abort a revision-drop transaction unless active coverage is complete.
pub fn stage_complete_embedding_coverage_guard(txn: &mut TxnBuilder, scope_id: &str) {
    txn.push(Statement::with_params(
        "INSERT INTO chunk (chunk_id)
         SELECT NULL
         WHERE EXISTS (
           SELECT 1 FROM memory
           WHERE memory.scope_id = ?1
             AND memory.status NOT IN ('erased', 'quarantined')
             AND NOT EXISTS (
               SELECT 1 FROM embedding
               WHERE embedding.scope_id = ?1
                 AND embedding.memory_id = memory.id
                 AND embedding.model_revision = (
                   SELECT revision FROM embedding_model WHERE active = 1
                 )
             )
         )",
        vec![Value::from(scope_id)],
    ));
}

/// Read one scope's embedding queue aggregates through the leader.
///
/// The result contains no memory records or identifiers. The join exists only
/// to distinguish work still held by quarantine from an admitted dead letter.
///
/// # Errors
///
/// Store errors or a missing aggregate row.
pub async fn embedding_queue_counts(
    store: &StoreHandle,
    scope_id: &str,
    namespace: &str,
    processor_glob: &str,
    revision_offset: i64,
) -> Result<EmbeddingQueueCounts, Error> {
    let rows: Vec<QueueRow> = store
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
               ON memory.scope_id = ?1
              AND memory.scope_id = processing.tenant
              AND memory.id = processing.key
             WHERE processing.state IN ('pending', 'claimed', 'failed', 'dead')
               AND processing.tenant = ?1
               AND processing.namespace = ?2
               AND processing.processor GLOB ?3
               AND substr(processing.processor, ?4) NOT GLOB '*[^0-9]*'",
            vec![
                Value::from(scope_id),
                Value::from(namespace),
                Value::from(processor_glob),
                Value::Integer(revision_offset),
            ],
        )
        .await?;
    let row = rows
        .first()
        .ok_or_else(|| Error::Integrity("embedding queue count returned no row".to_owned()))?;
    Ok(EmbeddingQueueCounts {
        pending: row.pending,
        dead: row.dead,
        quarantined: row.quarantined,
        oldest_pending_created_at: row.oldest_pending_created_at,
    })
}
