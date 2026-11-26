//! Persisted recall traces (spec 018 B-6).
//!
//! A trace is stored against a scope, keyed by its query hash, as the
//! body-free JSON of ids, scores, and ranks that [`RecallTrace::to_json`]
//! produces, and it is read back only until its retention window ends. The
//! delivery report of spec 025 (units, ranges, counts, and the token counter)
//! is stored beside it as its own document, so a reader can tell which units
//! a budget was enforced in (025 B-5, B-13); the delivered text never is.
//!
//! Every statement here names its scope. Nothing here is on the recall path:
//! a surface calls [`TraceStore::record`] after a query when persistence is
//! on, which it is not by default.

use aicortex_store::ScopeId;
use rahi_store::{Statement, StoreHandle, TxnBuilder, Value};
use rahi_types::{Error, UnixSeconds};
use ring::rand::{SecureRandom, SystemRandom};
use serde::Deserialize;

use crate::delivery::DeliveryReport;
use crate::trace::RecallTrace;

/// How long a persisted trace is read, in seconds, unless the deployment says
/// otherwise (D-10 of spec 018): thirty days, long enough to debug a ranking
/// complaint and to compare two evaluation runs.
pub const DEFAULT_RETENTION_SECS: u64 = 30 * 24 * 60 * 60;

/// The most traces one scope keeps; the oldest beyond it are removed when a
/// new one is recorded, so a busy scope cannot grow the table without bound.
pub const MAX_TRACES_PER_SCOPE: u32 = 10_000;

/// The most traces one read returns.
pub const MAX_TRACES_PER_READ: u32 = 50;

/// How many already-expired traces one record removes.
const EXPIRED_PER_RECORD: u32 = 500;

/// One stored trace, as a reader gets it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct StoredTrace {
    /// The row's id.
    pub trace_id: String,
    /// The query hash it is keyed by.
    pub query_hash: String,
    /// When it was recorded, in Unix seconds.
    pub recorded_at: u64,
    /// The instant after which it is no longer read, in Unix seconds.
    pub expires_at: u64,
    /// The trace document: ids, scores, and ranks.
    pub trace: serde_json::Value,
    /// The delivery report document, when the query delivered.
    pub delivery: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct Row {
    trace_id: String,
    query_hash: String,
    recorded_at: i64,
    expires_at: i64,
    trace: String,
    delivery: Option<String>,
}

impl Row {
    fn decode(self) -> Result<StoredTrace, Error> {
        let corrupt = |what: &str, error: serde_json::Error| {
            Error::Integrity(format!(
                "a stored recall trace's {what} is not JSON: {error}"
            ))
        };
        let seconds = |value: i64| {
            u64::try_from(value)
                .map_err(|_| Error::Integrity("a stored recall trace has a negative time".into()))
        };
        Ok(StoredTrace {
            recorded_at: seconds(self.recorded_at)?,
            expires_at: seconds(self.expires_at)?,
            trace: serde_json::from_str(&self.trace).map_err(|e| corrupt("trace", e))?,
            delivery: self
                .delivery
                .as_deref()
                .map(serde_json::from_str)
                .transpose()
                .map_err(|e| corrupt("delivery report", e))?,
            trace_id: self.trace_id,
            query_hash: self.query_hash,
        })
    }
}

/// The repository of persisted traces.
#[derive(Clone, Copy, Debug, Default)]
pub struct TraceStore;

impl TraceStore {
    /// Persist `trace` (and the `delivery` report, when the query delivered)
    /// in `scope`, readable for `retention_secs` from `now`, and return its id.
    ///
    /// The same transaction removes a bounded batch of the scope's expired
    /// traces and any beyond [`MAX_TRACES_PER_SCOPE`].
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] for a zero retention window, which would store a
    /// trace nobody can read; [`Error::Integrity`] when no entropy is
    /// available or the trace does not serialize; the store's error.
    pub async fn record(
        store: &StoreHandle,
        scope: &ScopeId,
        trace: &RecallTrace,
        delivery: Option<&DeliveryReport>,
        now: UnixSeconds,
        retention_secs: u64,
    ) -> Result<String, Error> {
        if retention_secs == 0 {
            return Err(Error::Validation(
                "the trace retention window must be at least one second".to_owned(),
            ));
        }
        let serialize = |error: serde_json::Error| Error::Integrity(error.to_string());
        let document = trace.to_json().map_err(serialize)?;
        let delivery = delivery
            .map(serde_json::to_string)
            .transpose()
            .map_err(serialize)?;
        let mut bytes = [0_u8; 16];
        SystemRandom::new()
            .fill(&mut bytes)
            .map_err(|_| Error::Integrity("no entropy for a trace id".to_owned()))?;
        let trace_id: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        let recorded = i64::try_from(now.get()).unwrap_or(i64::MAX);
        let expires = i64::try_from(now.get().saturating_add(retention_secs)).unwrap_or(i64::MAX);

        let mut txn = TxnBuilder::new();
        txn.push(Statement::with_params(
            "DELETE FROM recall_trace WHERE scope_id = ?1 AND trace_id IN (
                 SELECT trace_id FROM recall_trace
                 WHERE scope_id = ?1 AND expires_at <= ?2 LIMIT ?3)",
            vec![
                Value::from(scope.as_str()),
                Value::Integer(recorded),
                Value::from(EXPIRED_PER_RECORD),
            ],
        ));
        txn.push(Statement::with_params(
            "INSERT INTO recall_trace
             (trace_id, scope_id, query_hash, recorded_at, expires_at, trace, delivery)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            vec![
                Value::from(trace_id.as_str()),
                Value::from(scope.as_str()),
                Value::from(trace.query_hash.as_str()),
                Value::Integer(recorded),
                Value::Integer(expires),
                Value::from(document),
                delivery.map_or(Value::Null, Value::from),
            ],
        ));
        txn.push(Statement::with_params(
            "DELETE FROM recall_trace WHERE scope_id = ?1 AND trace_id IN (
                 SELECT trace_id FROM recall_trace WHERE scope_id = ?1
                 ORDER BY recorded_at DESC, trace_id DESC LIMIT -1 OFFSET ?2)",
            vec![
                Value::from(scope.as_str()),
                Value::from(MAX_TRACES_PER_SCOPE),
            ],
        ));
        store.txn(txn.into_statements()).await?;
        Ok(trace_id)
    }

    /// The unexpired traces of `scope` recorded for `query_hash`, newest
    /// first, at most `limit` (capped at [`MAX_TRACES_PER_READ`]).
    ///
    /// # Errors
    ///
    /// The store's error, or [`Error::Integrity`] when a stored document does
    /// not read back.
    pub async fn by_hash(
        store: &StoreHandle,
        scope: &ScopeId,
        query_hash: &str,
        now: UnixSeconds,
        limit: u32,
    ) -> Result<Vec<StoredTrace>, Error> {
        let rows: Vec<Row> = store
            .query(
                "SELECT trace_id, query_hash, recorded_at, expires_at, trace, delivery
                 FROM recall_trace
                 WHERE scope_id = ?1 AND query_hash = ?2 AND expires_at > ?3
                 ORDER BY recorded_at DESC, trace_id DESC LIMIT ?4"
                    .to_owned(),
                vec![
                    Value::from(scope.as_str()),
                    Value::from(query_hash),
                    Value::Integer(i64::try_from(now.get()).unwrap_or(i64::MAX)),
                    Value::from(limit.clamp(1, MAX_TRACES_PER_READ)),
                ],
            )
            .await?;
        rows.into_iter().map(Row::decode).collect()
    }
}
