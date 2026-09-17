//! Counters, not scans (spec 012 B-6).
//!
//! The predecessor's statistics endpoint read every row's metadata into
//! memory to count things (`openbrain://stats-loads-everything`), so the cost
//! of asking how many memories a scope holds grew with how many it held.
//! Here every aggregate is a row in `scope_counter`, updated by the same
//! transaction as the row that changes it, and `stats` is one statement over
//! at most one row per kind and status.
//!
//! No aggregate in this system is computed by reading bodies. A future
//! aggregate is a column or a row in this table, maintained on the write
//! path, never a `SELECT count(*)` over `memory`.

use std::collections::BTreeMap;

use aicortex_types::{MemoryKind, Scope, Status};
use rahi_store::{Statement, StoreHandle, TxnBuilder, Value};
use rahi_types::Error;
use serde::Deserialize;

use crate::scope_repo::ScopeId;

/// The maintained aggregates of one scope.
///
/// Bounded by the taxonomies rather than by the data: at most one entry per
/// [`MemoryKind`] and one per [`Status`], however many memories the scope
/// holds.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScopeStats {
    /// Every memory the scope holds, in any state.
    pub total: u64,
    /// How many of each kind.
    pub by_kind: BTreeMap<String, u64>,
    /// How many in each state.
    pub by_status: BTreeMap<String, u64>,
}

/// The `scope_counter` table.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counters;

/// One upsert: the counter for this scope, kind and status moves by `delta`.
///
/// `ON CONFLICT ... DO UPDATE` rather than a read followed by a write,
/// because a read outside the transaction would be a lost update under a
/// concurrent capture and a read inside it would be a second round-trip.
const ADJUST_SQL: &str = "INSERT INTO scope_counter (scope_id, kind, status, count)
    VALUES ($1, $2, $3, $4)
    ON CONFLICT (scope_id, kind, status)
    DO UPDATE SET count = max(0, scope_counter.count + $4)";

/// The whole of `stats` (B-6): one statement, one scope, no memory table.
const STATS_SQL: &str = "SELECT kind, status, count FROM scope_counter WHERE scope_id = $1";

#[derive(Debug, Deserialize)]
struct CounterRow {
    kind: String,
    status: String,
    count: i64,
}

impl Counters {
    /// Stage the counter move for a memory entering `status` (B-4, B-6).
    ///
    /// Staged into the caller's batch, never executed on its own: a counter
    /// that commits without its row, or a row that commits without its
    /// counter, is the defect this table exists to avoid.
    pub fn adjust(
        txn: &mut TxnBuilder,
        scope: &ScopeId,
        kind: MemoryKind,
        status: &Status,
        delta: i64,
    ) {
        txn.push(Statement::with_params(
            ADJUST_SQL,
            vec![
                Value::from(scope),
                Value::from(kind.label()),
                Value::from(status.label()),
                Value::Integer(delta),
            ],
        ));
    }

    /// Stage the counter move for one new memory.
    pub fn increment(txn: &mut TxnBuilder, scope: &ScopeId, kind: MemoryKind, status: &Status) {
        Self::adjust(txn, scope, kind, status, 1);
    }

    /// The statement `stats` runs, for a test that asserts what it costs.
    ///
    /// Exposed because FR-006 is about the shape of the read rather than
    /// about its answer: a test inspects this statement's plan and asserts it
    /// touches only `scope_counter`.
    #[must_use]
    pub fn stats_statement(scope: &Scope) -> Statement {
        Statement::with_params(STATS_SQL, vec![Value::from(&ScopeId::of(scope))])
    }

    /// The scope's aggregates (B-6).
    ///
    /// `query`, the local replica: a count is a report, and a report that is
    /// one replication lag behind is right about what it says it is. The
    /// leader round-trip belongs to the admission and uniqueness reads (B-5),
    /// where a stale answer would be a wrong decision rather than a slightly
    /// old number.
    ///
    /// # Errors
    ///
    /// The store's error, or [`Error::Integrity`] when a counter row holds a
    /// negative count.
    pub async fn stats(store: &StoreHandle, scope: &Scope) -> Result<ScopeStats, Error> {
        let statement = Self::stats_statement(scope);
        let rows: Vec<CounterRow> = store.query(statement.sql, statement.params).await?;
        let mut stats = ScopeStats::default();
        for row in rows {
            let count = u64::try_from(row.count).map_err(|_| {
                Error::Integrity(format!(
                    "scope_counter holds a negative count for {}/{}",
                    row.kind, row.status
                ))
            })?;
            stats.total = stats.total.saturating_add(count);
            let by_kind = stats.by_kind.entry(row.kind).or_default();
            *by_kind = by_kind.saturating_add(count);
            let by_status = stats.by_status.entry(row.status).or_default();
            *by_status = by_status.saturating_add(count);
        }
        Ok(stats)
    }
}
