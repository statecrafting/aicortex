//! Provenance and its derivation rows (spec 012 B-2, B-3, B-4).
//!
//! Provenance is not optional here for the same reason it is not optional in
//! the record (011 B-8, constitution IX): a claim whose origin is unknown is
//! not a memory, it is a string. The table exists as well as the record
//! because provenance is queried (which import produced this, what was
//! derived from what), and a question asked of a document is a scan.
//!
//! `derived_from` is a child table rather than a list inside the row, so the
//! derivation graph is a join away rather than a parse away, and so erasure
//! (014) can reach the derivatives of an erased memory. Every derivation row
//! carries the child's `scope_id`: a derivation is read under a scope
//! predicate like everything else, and a parent that lives in another scope
//! is not reachable through it (B-9).

use aicortex_types::{ExtractorVersion, MemoryId, Provenance, Scope, SourceRef, SourceSystem};
use rahi_store::{Statement, StoreHandle, TxnBuilder, Value};
use rahi_types::{Error, UnixSeconds};
use serde::Deserialize;

use crate::scope_repo::{ScopeId, seconds_to_sql};

/// The `provenance` and `memory_derivation` tables.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProvenanceRepo;

const INSERT_SQL: &str = "INSERT INTO provenance (
        memory_id, scope_id, source_system, source_external_id, source_locator,
        captured_at, ingested_at, extractor_name, extractor_version
    ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)";

const DERIVATION_SQL: &str =
    "INSERT INTO memory_derivation (memory_id, parent_id, position, scope_id)
    VALUES ($1, $2, $3, $4)";

const GET_SQL: &str = "SELECT source_system, source_external_id, source_locator,
        captured_at, ingested_at, extractor_name, extractor_version
    FROM provenance WHERE scope_id = $1 AND memory_id = $2";

/// The parents of one memory, in the order they were named.
///
/// The scope predicate is on the derivation row, and the `EXISTS` clause is
/// scoped too: a parent id that names a memory in another scope is not
/// returned, because a cross-scope derivation is not a relationship this
/// system exposes (B-9).
const PARENTS_SQL: &str = "SELECT parent_id FROM memory_derivation
    WHERE scope_id = $1 AND memory_id = $2
      AND EXISTS (SELECT 1 FROM memory
                  WHERE memory.scope_id = $1 AND memory.id = memory_derivation.parent_id)
    ORDER BY position";

#[derive(Debug, Deserialize)]
struct ProvenanceRow {
    source_system: String,
    source_external_id: Option<String>,
    source_locator: Option<String>,
    captured_at: i64,
    ingested_at: i64,
    extractor_name: Option<String>,
    extractor_version: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ParentRow {
    parent_id: String,
}

impl ProvenanceRepo {
    /// Stage the provenance row and one row per parent (B-4).
    ///
    /// Appended to the caller's batch, so they commit with the memory they
    /// describe or not at all. A memory whose provenance row failed to land
    /// would be exactly the record 011 made unconstructible, reintroduced at
    /// the storage layer.
    pub fn stage(
        txn: &mut TxnBuilder,
        scope: &ScopeId,
        memory: MemoryId,
        provenance: &Provenance,
    ) -> Result<(), Error> {
        let (extractor_name, extractor_version) =
            provenance
                .extractor
                .as_ref()
                .map_or((None, None), |extractor| {
                    (
                        Some(extractor.name.clone()),
                        Some(extractor.version.clone()),
                    )
                });
        txn.push(Statement::with_params(
            INSERT_SQL,
            vec![
                Value::from(memory.to_string()),
                Value::from(scope),
                Value::from(provenance.source.system.as_str()),
                Value::from(provenance.source.external_id.clone()),
                Value::from(provenance.source.locator.clone()),
                Value::Integer(seconds_to_sql(provenance.captured_at)),
                Value::Integer(seconds_to_sql(provenance.ingested_at)),
                Value::from(extractor_name),
                Value::from(extractor_version),
            ],
        ));
        for (position, parent) in provenance.derived_from.iter().enumerate() {
            let position = i64::try_from(position).map_err(|_| {
                Error::Validation(format!(
                    "memory {memory} names more parents than a position can number"
                ))
            })?;
            txn.push(Statement::with_params(
                DERIVATION_SQL,
                vec![
                    Value::from(memory.to_string()),
                    Value::from(parent.to_string()),
                    Value::Integer(position),
                    Value::from(scope),
                ],
            ));
        }
        Ok(())
    }

    /// One memory's provenance, rebuilt from its row and its parents.
    ///
    /// `query`, the local replica: this is a detail read that accompanies the
    /// memory itself, and nothing is decided on it (B-5).
    ///
    /// # Errors
    ///
    /// The store's error, or [`Error::Integrity`] when the row holds a
    /// timestamp, a source system, or a parent id this crate cannot read
    /// back.
    pub async fn get(
        store: &StoreHandle,
        scope: &Scope,
        memory: MemoryId,
    ) -> Result<Option<Provenance>, Error> {
        let scope_id = ScopeId::of(scope);
        let rows: Vec<ProvenanceRow> = store
            .query(
                GET_SQL,
                vec![Value::from(&scope_id), Value::from(memory.to_string())],
            )
            .await?;
        let Some(row) = rows.into_iter().next() else {
            return Ok(None);
        };
        let parents = Self::parents(store, scope, memory).await?;
        row_to_provenance(memory, row, parents).map(Some)
    }

    /// The memories this one was derived from, in order, within this scope.
    ///
    /// `query`, the local replica: the same read as the detail above, and for
    /// the same reason (B-5).
    ///
    /// # Errors
    ///
    /// The store's error, or [`Error::Integrity`] when a row holds a parent
    /// id that is not a memory id.
    pub async fn parents(
        store: &StoreHandle,
        scope: &Scope,
        memory: MemoryId,
    ) -> Result<Vec<MemoryId>, Error> {
        let scope_id = ScopeId::of(scope);
        let rows: Vec<ParentRow> = store
            .query(
                PARENTS_SQL,
                vec![Value::from(&scope_id), Value::from(memory.to_string())],
            )
            .await?;
        rows.into_iter()
            .map(|row| {
                MemoryId::parse(&row.parent_id).map_err(|error| {
                    Error::Integrity(format!(
                        "memory_derivation holds a parent that is not a memory id: {error}"
                    ))
                })
            })
            .collect()
    }
}

/// Rebuild a [`Provenance`] from its row and the parents already read.
///
/// The operator-override marker of spec 013 B-5 is not among the columns and
/// comes back `None` here. That is the projection behaving as spec 012 D-3
/// describes every column: the memory's own record is the single source of
/// truth for what it says, and this table exists for the facts that are
/// *queried* (which import produced this, what was derived from what). A
/// reviewer looking for memories admitted over a refusal reads the record
/// through `MemoryRepo::get`, where the marker is (013 D-6).
fn row_to_provenance(
    memory: MemoryId,
    row: ProvenanceRow,
    derived_from: Vec<MemoryId>,
) -> Result<Provenance, Error> {
    let seconds = |value: i64, field: &str| -> Result<UnixSeconds, Error> {
        u64::try_from(value).map(UnixSeconds::new).map_err(|_| {
            Error::Integrity(format!(
                "provenance row for {memory} holds a negative {field}"
            ))
        })
    };
    let mut source = SourceRef::new(SourceSystem::new(row.source_system)?);
    if let Some(external_id) = row.source_external_id {
        source = source.with_external_id(external_id);
    }
    if let Some(locator) = row.source_locator {
        source = source.with_locator(locator);
    }
    let extractor = match (row.extractor_name, row.extractor_version) {
        (Some(name), Some(version)) => Some(ExtractorVersion::new(name, version)?),
        (None, None) => None,
        (name, version) => {
            return Err(Error::Integrity(format!(
                "provenance row for {memory} names half an extractor: {name:?}/{version:?}"
            )));
        }
    };
    Ok(Provenance {
        source,
        captured_at: seconds(row.captured_at, "captured_at")?,
        ingested_at: seconds(row.ingested_at, "ingested_at")?,
        derived_from,
        extractor,
        admission: None,
        spans: Vec::new(),
    })
}
