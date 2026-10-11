//! The model revision registry and coverage report.

use aicortex_types::MemoryId;
use rahi_store::{Blob, Statement, StoreHandle, TxnBuilder, Value};
use rahi_types::{Error, UnixSeconds};
use serde::Deserialize;

use aicortex_store::{
    ReadStore, embedding_coverage, live_memory_total, stage_complete_embedding_coverage_guard,
};

use crate::provider::{EmbeddingProvider, ModelId, Vector};

/// One immutable model revision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelRevision {
    /// Provider model identity.
    pub model_id: ModelId,
    /// Monotonic configuration revision.
    pub revision: u32,
    /// Vector width.
    pub dims: u16,
    /// Whether the provider returns L2-normalized values.
    pub normalized: bool,
    /// First activation time.
    pub first_seen: UnixSeconds,
    /// Whether queries should use this revision now.
    pub active: bool,
}

impl ModelRevision {
    /// Build a revision from the resolved provider.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] when revision or dimensions are zero.
    pub fn from_provider(
        provider: &impl EmbeddingProvider,
        revision: u32,
        first_seen: UnixSeconds,
        active: bool,
    ) -> Result<Self, Error> {
        if revision == 0 || provider.dims() == 0 {
            return Err(Error::Validation(
                "embedding model revision and dimensions must be non-zero".to_owned(),
            ));
        }
        Ok(Self {
            model_id: provider.id(),
            revision,
            dims: provider.dims(),
            normalized: provider.normalized(),
            first_seen,
            active,
        })
    }
}

/// Coverage for one known revision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Coverage {
    /// Model revision.
    pub revision: u32,
    /// Live memories with at least one vector at this revision.
    pub embedded: u64,
    /// All live memories eligible for embedding.
    pub total: u64,
}

/// One stored chunk vector read back under an explicit model revision.
#[derive(Clone, Debug, PartialEq)]
pub struct StoredVector {
    /// The memory the vector belongs to.
    pub memory_id: MemoryId,
    /// The chunk the vector was computed from.
    pub chunk_ordinal: u32,
    /// The decoded vector.
    pub vector: Vector,
}

/// Registry statements and reads.
#[derive(Clone, Copy, Debug, Default)]
pub struct ModelRegistry;

#[derive(Debug, Deserialize)]
struct ModelRow {
    model_id: String,
    revision: i64,
    dims: i64,
    normalized: i64,
    first_seen: i64,
    active: i64,
}

#[derive(Debug, Deserialize)]
struct VectorRow {
    memory_id: String,
    chunk_ordinal: i64,
    dims: i64,
    vector: Blob,
}

#[derive(Debug, Deserialize)]
struct LatestRow {
    revision: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct CoverageRow {
    revision: i64,
}

impl ModelRegistry {
    /// Stage activation atomically.
    ///
    /// An existing identity must retain the same model, dimensions, and
    /// normalization. Its recorded first-seen instant is preserved when the
    /// same revision is activated again after a restart. An identity mismatch
    /// provokes a constraint failure instead of silently reinterpreting stored
    /// vectors.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] when the supplied revision is not active.
    pub fn activate(txn: &mut TxnBuilder, model: &ModelRevision) -> Result<(), Error> {
        if !model.active {
            return Err(Error::Validation(
                "cannot activate a model revision marked inactive".to_owned(),
            ));
        }
        txn.push(Statement::with_params(
            "UPDATE embedding_model
             SET active = CASE WHEN revision <= ?1 THEN 0 ELSE NULL END
             WHERE active = 1",
            vec![Value::Integer(i64::from(model.revision))],
        ));
        txn.push(Statement::with_params(
            "INSERT INTO embedding_model
             (model_id, revision, dims, normalized, first_seen, active)
             VALUES (
               ?1, ?2, ?3, ?4, ?5,
               CASE WHEN EXISTS (
                 SELECT 1 FROM embedding_model WHERE revision > ?2
               ) THEN NULL ELSE 1 END
             )
             ON CONFLICT(revision) DO UPDATE SET
               active = CASE
                 WHEN model_id = ?1 AND dims = ?3 AND normalized = ?4 THEN 1
                 ELSE NULL END",
            vec![
                Value::from(model.model_id.as_str()),
                Value::Integer(i64::from(model.revision)),
                Value::Integer(i64::from(model.dims)),
                Value::from(model.normalized),
                Value::Integer(seconds_to_sql(model.first_seen)),
            ],
        ));
        Ok(())
    }

    /// Read the active revision through the leader.
    ///
    /// # Errors
    ///
    /// Store errors or an invalid registry row.
    pub async fn active(store: &impl ReadStore) -> Result<Option<ModelRevision>, Error> {
        let rows: Vec<ModelRow> = store
            .query_consistent(
                "SELECT model_id, revision, dims, normalized, first_seen, active
                 FROM embedding_model WHERE active = 1",
                vec![],
            )
            .await?;
        rows.into_iter().next().map(model_from_row).transpose()
    }

    /// The highest revision ever recorded, active or not.
    ///
    /// # Errors
    ///
    /// Store errors or a revision outside `u32`.
    pub async fn latest_revision(store: &StoreHandle) -> Result<Option<u32>, Error> {
        let rows: Vec<LatestRow> = store
            .query_consistent(
                "SELECT MAX(revision) AS revision FROM embedding_model",
                vec![],
            )
            .await?;
        rows.into_iter()
            .next()
            .and_then(|row| row.revision)
            .map(|revision| {
                u32::try_from(revision)
                    .map_err(|_| Error::Integrity("model revision is outside u32".to_owned()))
            })
            .transpose()
    }

    /// Read stored vectors comparable to a query embedded under `query`.
    ///
    /// This is the revision predicate of B-8: the caller names the revision
    /// its query vector was produced under, and the statement matches rows
    /// by revision, model identity, width, and normalization together, so a
    /// vector from another revision cannot be returned and therefore cannot
    /// be compared. Rows come in `(memory_id, chunk_ordinal)` order; pass the
    /// last row's pair as `after` for the next page.
    ///
    /// # Errors
    ///
    /// Store errors, or [`Error::Integrity`] for a row whose bytes disagree
    /// with its recorded width.
    pub async fn vectors(
        store: &StoreHandle,
        scope_id: &str,
        query: &ModelRevision,
        after: Option<(MemoryId, u32)>,
        limit: u32,
    ) -> Result<Vec<StoredVector>, Error> {
        let (after_memory, after_ordinal) = after.map_or((String::new(), -1), |(id, ordinal)| {
            (id.to_string(), i64::from(ordinal))
        });
        let rows: Vec<VectorRow> = store
            .query_consistent(
                "SELECT memory_id, chunk_ordinal, dims, vector FROM embedding
                 WHERE scope_id = ?1 AND model_revision = ?2 AND model_id = ?3
                   AND dims = ?4 AND normalized = ?5
                   AND (memory_id > ?6 OR (memory_id = ?6 AND chunk_ordinal > ?7))
                 ORDER BY memory_id, chunk_ordinal LIMIT ?8",
                vec![
                    Value::from(scope_id),
                    Value::Integer(i64::from(query.revision)),
                    Value::from(query.model_id.as_str()),
                    Value::Integer(i64::from(query.dims)),
                    Value::from(query.normalized),
                    Value::from(after_memory),
                    Value::Integer(after_ordinal),
                    Value::Integer(i64::from(limit)),
                ],
            )
            .await?;
        rows.into_iter()
            .map(|row| {
                let dims = u16::try_from(row.dims)
                    .map_err(|_| Error::Integrity("vector width is outside u16".to_owned()))?;
                Ok(StoredVector {
                    memory_id: row.memory_id.parse().map_err(|error| {
                        Error::Integrity(format!("stored memory id is invalid: {error}"))
                    })?,
                    chunk_ordinal: u32::try_from(row.chunk_ordinal)
                        .map_err(|_| Error::Integrity("chunk ordinal is outside u32".to_owned()))?,
                    vector: Vector::from_le_bytes(row.vector.as_slice(), dims)?,
                })
            })
            .collect()
    }

    /// Coverage of every model revision, including zero-coverage revisions.
    ///
    /// The predicate joins embeddings by explicit revision. Query code uses
    /// the same revision predicate and never compares across revisions.
    ///
    /// # Errors
    ///
    /// Store errors or negative counts in a corrupted row.
    pub async fn coverage(store: &impl ReadStore, scope_id: &str) -> Result<Vec<Coverage>, Error> {
        let total = live_memory_total(store, scope_id).await?;
        let embedded = embedding_coverage(store, scope_id)
            .await?
            .into_iter()
            .collect::<std::collections::BTreeMap<_, _>>();
        let rows: Vec<CoverageRow> = store
            .query(
                "SELECT revision FROM embedding_model ORDER BY revision",
                vec![],
            )
            .await?;
        rows.into_iter()
            .map(|row| {
                let revision = u32::try_from(row.revision)
                    .map_err(|_| Error::Integrity("coverage revision is outside u32".to_owned()))?;
                Ok(Coverage {
                    revision,
                    embedded: embedded.get(&revision).copied().unwrap_or(0),
                    total,
                })
            })
            .collect()
    }

    /// Count live memories eligible for embedding, even before a model exists.
    ///
    /// # Errors
    ///
    /// Store errors or a negative count in a corrupted row.
    pub async fn live_total(store: &impl ReadStore, scope_id: &str) -> Result<u64, Error> {
        live_memory_total(store, scope_id).await
    }

    /// Remove one inactive revision only after active coverage is complete.
    pub async fn drop_revision(
        store: &StoreHandle,
        scope_id: &str,
        revision: u32,
    ) -> Result<(), Error> {
        let active = Self::active(store)
            .await?
            .ok_or_else(|| Error::Config("no active embedding model".to_owned()))?;
        if active.revision == revision {
            return Err(Error::Conflict(format!(
                "cannot drop active embedding revision {revision}"
            )));
        }
        let coverage = Self::coverage(store, scope_id).await?;
        let active_coverage = coverage
            .iter()
            .find(|item| item.revision == active.revision)
            .ok_or_else(|| Error::Integrity("active revision has no coverage row".to_owned()))?;
        if active_coverage.embedded != active_coverage.total {
            return Err(Error::Conflict(format!(
                "active revision {} covers {}/{} live memories",
                active.revision, active_coverage.embedded, active_coverage.total
            )));
        }
        let mut txn = TxnBuilder::new();
        stage_complete_embedding_coverage_guard(&mut txn, scope_id);
        txn.push(Statement::with_params(
            "UPDATE embedding_model SET revision = revision
             WHERE revision = ?1 AND active = 0",
            vec![Value::Integer(i64::from(revision))],
        ));
        for sql in [
            "DELETE FROM embedding WHERE scope_id = ?1 AND model_revision = ?2
             AND EXISTS (SELECT 1 FROM embedding_model
                         WHERE revision = ?2 AND active = 0)",
            "DELETE FROM chunk WHERE scope_id = ?1 AND model_revision = ?2
             AND EXISTS (SELECT 1 FROM embedding_model
                         WHERE revision = ?2 AND active = 0)",
        ] {
            txn.push(Statement::with_params(
                sql,
                vec![Value::from(scope_id), Value::Integer(i64::from(revision))],
            ));
        }
        let results = store.txn(txn.into_statements()).await?;
        if results
            .get(1)
            .is_none_or(|result| result.rows_affected != 1)
        {
            return Err(Error::Conflict(format!(
                "embedding revision {revision} is not inactive or active coverage changed"
            )));
        }
        Ok(())
    }
}

impl std::fmt::Display for Coverage {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{}:{}/{}",
            self.revision, self.embedded, self.total
        )
    }
}

fn model_from_row(row: ModelRow) -> Result<ModelRevision, Error> {
    Ok(ModelRevision {
        model_id: ModelId::new(row.model_id)
            .map_err(|error| Error::Integrity(error.message().to_owned()))?,
        revision: u32::try_from(row.revision)
            .map_err(|_| Error::Integrity("model revision is outside u32".to_owned()))?,
        dims: u16::try_from(row.dims)
            .map_err(|_| Error::Integrity("model dimensions are outside u16".to_owned()))?,
        normalized: match row.normalized {
            0 => false,
            1 => true,
            _ => {
                return Err(Error::Integrity(
                    "model normalized flag is not zero or one".to_owned(),
                ));
            }
        },
        first_seen: UnixSeconds::new(
            u64::try_from(row.first_seen)
                .map_err(|_| Error::Integrity("model first_seen is negative".to_owned()))?,
        ),
        active: row.active == 1,
    })
}

pub(crate) fn seconds_to_sql(value: UnixSeconds) -> i64 {
    i64::try_from(value.get()).unwrap_or(i64::MAX)
}
