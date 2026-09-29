//! The model revision registry and coverage report.

use rahi_store::{Statement, StoreHandle, TxnBuilder, Value};
use rahi_types::{Error, UnixSeconds};
use serde::Deserialize;

use crate::provider::{EmbeddingProvider, ModelId};

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
struct CoverageRow {
    revision: i64,
    embedded: i64,
    total: i64,
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
    pub async fn active(store: &StoreHandle) -> Result<Option<ModelRevision>, Error> {
        let rows: Vec<ModelRow> = store
            .query_consistent(
                "SELECT model_id, revision, dims, normalized, first_seen, active
                 FROM embedding_model WHERE active = 1",
                vec![],
            )
            .await?;
        rows.into_iter().next().map(model_from_row).transpose()
    }

    /// Coverage of every model revision, including zero-coverage revisions.
    ///
    /// The predicate joins embeddings by explicit revision. Query code uses
    /// the same revision predicate and never compares across revisions.
    ///
    /// # Errors
    ///
    /// Store errors or negative counts in a corrupted row.
    pub async fn coverage(store: &StoreHandle, scope_id: &str) -> Result<Vec<Coverage>, Error> {
        let rows: Vec<CoverageRow> = store
            .query(
                "SELECT model.revision AS revision,
                    COUNT(DISTINCT live.id) AS embedded,
                    (SELECT COUNT(*) FROM memory
                     WHERE scope_id = ?1
                       AND status NOT IN ('erased', 'quarantined')) AS total
                 FROM embedding_model model
                 LEFT JOIN embedding ON embedding.model_revision = model.revision
                    AND embedding.scope_id = ?1
                 LEFT JOIN memory live ON live.scope_id = embedding.scope_id
                    AND live.id = embedding.memory_id
                    AND live.status NOT IN ('erased', 'quarantined')
                 GROUP BY model.revision ORDER BY model.revision",
                vec![Value::from(scope_id)],
            )
            .await?;
        rows.into_iter().map(coverage_from_row).collect()
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
        txn.push(Statement::with_params(
            "UPDATE embedding_model SET revision = revision
             WHERE revision = ?2 AND active = 0
             AND NOT EXISTS (
               SELECT 1 FROM memory m
               WHERE m.scope_id = ?1
                 AND m.status NOT IN ('erased', 'quarantined')
                 AND NOT EXISTS (
                   SELECT 1 FROM embedding e
                   WHERE e.scope_id = ?1 AND e.memory_id = m.id
                     AND e.model_revision = (SELECT revision FROM embedding_model WHERE active = 1)
                 )
             )",
            vec![Value::from(scope_id), Value::Integer(i64::from(revision))],
        ));
        for sql in [
            "DELETE FROM embedding WHERE scope_id = ?1 AND model_revision = ?2
             AND EXISTS (SELECT 1 FROM embedding_model
                         WHERE revision = ?2 AND active = 0)
             AND NOT EXISTS (
               SELECT 1 FROM memory m
               WHERE m.scope_id = ?1
                 AND m.status NOT IN ('erased', 'quarantined')
                 AND NOT EXISTS (
                   SELECT 1 FROM embedding e
                   WHERE e.scope_id = ?1 AND e.memory_id = m.id
                     AND e.model_revision = (SELECT revision FROM embedding_model WHERE active = 1)
                 )
             )",
            "DELETE FROM chunk WHERE scope_id = ?1 AND model_revision = ?2
             AND EXISTS (SELECT 1 FROM embedding_model
                         WHERE revision = ?2 AND active = 0)
             AND NOT EXISTS (
               SELECT 1 FROM memory m
               WHERE m.scope_id = ?1
                 AND m.status NOT IN ('erased', 'quarantined')
                 AND NOT EXISTS (
                   SELECT 1 FROM embedding e
                   WHERE e.scope_id = ?1 AND e.memory_id = m.id
                     AND e.model_revision = (SELECT revision FROM embedding_model WHERE active = 1)
                 )
             )",
        ] {
            txn.push(Statement::with_params(
                sql,
                vec![Value::from(scope_id), Value::Integer(i64::from(revision))],
            ));
        }
        let results = store.txn(txn.into_statements()).await?;
        if results
            .first()
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

fn coverage_from_row(row: CoverageRow) -> Result<Coverage, Error> {
    Ok(Coverage {
        revision: u32::try_from(row.revision)
            .map_err(|_| Error::Integrity("coverage revision is outside u32".to_owned()))?,
        embedded: u64::try_from(row.embedded)
            .map_err(|_| Error::Integrity("embedded coverage is negative".to_owned()))?,
        total: u64::try_from(row.total)
            .map_err(|_| Error::Integrity("total coverage is negative".to_owned()))?,
    })
}

pub(crate) fn seconds_to_sql(value: UnixSeconds) -> i64 {
    i64::try_from(value.get()).unwrap_or(i64::MAX)
}
