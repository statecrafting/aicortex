//! The paged cosine scan (spec 016 B-2, B-4, B-7, B-8).
//!
//! [`scan`] reads one scope's vectors at one model revision through rahi's
//! paged read, a page at a time, scores each row against the normalized
//! query, and keeps the best `k` in a [`TopK`]. A page is dropped before the
//! next is fetched, so peak memory is one page plus `k` candidates however
//! large the scope grows.
//!
//! Everything that decides *which* rows are eligible is a predicate in the
//! statement: the scope, the model revision together with the model
//! identity, width and normalization spec 015 D-26 compares on, and the
//! memory status filter. Nothing is fetched and then discarded in process.
//!
//! The scan carries a soft budget. When it runs out with rows still unread,
//! the scan stops after the page in hand and returns what it has, marked
//! [`ScanOutcome::truncated`] (B-8). It never stops silently.

use std::future::Future;
use std::time::{Duration, Instant};

use aicortex_store::ScopeId;
use aicortex_types::{MemoryId, Status};
use rahi_store::{Blob, DEFAULT_PAGE_ROWS, Page, StoreHandle, Value};
use rahi_types::Error;
use serde::Deserialize;

use crate::topk::{Candidate, TopK};
use crate::vector::{VectorError, decode_into, dot, normalize};

/// The scan's default soft budget (B-8, FR-007; D-6).
///
/// A ceiling, not a target: an optimized scan of 20000 chunks at 384
/// dimensions takes tens of milliseconds (the benchmark's table records
/// it), and this leaves room for the unoptimized test build FR-007 is
/// asserted in on a shared CI runner. Spec 018 passes its own per-query
/// budget.
pub const DEFAULT_BUDGET: Duration = Duration::from_secs(2);

/// How a scan pages and how long it may take.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScanConfig {
    /// Rows per page. Peak memory is one page of rows plus `k` candidates.
    pub page_size: u32,
    /// The soft deadline, measured from the start of the scan (B-8).
    pub budget: Duration,
}

impl Default for ScanConfig {
    fn default() -> Self {
        Self {
            page_size: DEFAULT_PAGE_ROWS,
            budget: DEFAULT_BUDGET,
        }
    }
}

/// The memory statuses a scan admits, as a predicate in its statement.
///
/// [`Filter::retrievable`] is what retrieval sees without being asked to
/// widen: active memories only, so quarantined content is invisible by
/// default. Erased memories have no vectors to find.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Filter {
    statuses: Vec<&'static str>,
}

impl Filter {
    /// Active memories only.
    #[must_use]
    pub fn retrievable() -> Self {
        Self {
            statuses: vec![Status::Active.label()],
        }
    }

    /// Exactly the named statuses.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] when the set is empty: a scan that admits no
    /// status could only ever return nothing.
    pub fn statuses(statuses: impl IntoIterator<Item = Status>) -> Result<Self, Error> {
        let mut labels: Vec<&'static str> = statuses.into_iter().map(Status::label).collect();
        labels.sort_unstable();
        labels.dedup();
        if labels.is_empty() {
            return Err(Error::Validation(
                "a scan filter must admit at least one memory status".to_owned(),
            ));
        }
        Ok(Self { statuses: labels })
    }

    /// The admitted status labels, sorted.
    #[must_use]
    pub fn labels(&self) -> &[&'static str] {
        &self.statuses
    }
}

impl Default for Filter {
    fn default() -> Self {
        Self::retrievable()
    }
}

/// What a stored vector must match to be compared with a query.
///
/// The four fields spec 015 records beside every vector and compares on
/// together (015 B-8, D-26): a vector of another revision, model, width, or
/// normalization is never returned, so it is never compared. Spec 015's
/// `ModelRevision` supplies them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Comparable<'a> {
    /// The provider's model identity.
    pub model_id: &'a str,
    /// The model revision the query vector was embedded under.
    pub revision: u32,
    /// The vector width.
    pub dims: u16,
    /// Whether the model's vectors are L2-normalized.
    pub normalized: bool,
}

/// One scan request.
#[derive(Clone, Copy, Debug)]
pub struct ScanQuery<'a> {
    /// The scope searched; no row of another scope is ever read.
    pub scope: &'a ScopeId,
    /// The model the query vector was embedded under.
    pub model: Comparable<'a>,
    /// The query vector. It is normalized before scoring.
    pub vector: &'a [f32],
    /// How many candidates to return at most.
    pub k: usize,
    /// The memory statuses admitted.
    pub filter: &'a Filter,
}

/// What one scan found.
#[derive(Clone, Debug, PartialEq)]
pub struct ScanOutcome {
    /// The best candidates, best first: score descending, chunk id ascending.
    pub candidates: Vec<Candidate>,
    /// Whether the budget ran out with rows possibly unread (B-8).
    pub truncated: bool,
    /// Rows scored.
    pub scanned: u64,
    /// Pages read that returned rows; the empty read that ends a scan on an
    /// exact page boundary is not counted.
    pub pages: u32,
}

/// One stored vector row as the scan reads it.
#[derive(Clone, Debug, Deserialize)]
pub struct VectorRow {
    /// The chunk the vector was computed from.
    pub chunk_id: String,
    /// The memory the chunk belongs to.
    pub memory_id: String,
    /// The chunk's position within its memory.
    pub chunk_ordinal: i64,
    /// The recorded width.
    pub dims: i64,
    /// The `f32` little-endian values.
    pub vector: Blob,
}

/// Where a scan's pages come from.
///
/// Pages are in `(memory_id, chunk_ordinal)` order. `after` is the key of
/// the last row of the previous page, and a page shorter than `size` is the
/// last. [`StoreRows`] is the store-backed source; the trait exists so the
/// allocation bound of FR-002 can be measured on the scan alone.
pub trait PageSource {
    /// Fetch up to `size` rows after `after`.
    fn page(
        &mut self,
        after: Option<(&str, i64)>,
        size: u32,
    ) -> impl Future<Output = Result<Vec<VectorRow>, Error>> + Send;
}

/// The store-backed page source: one scope, one revision, one filter.
#[derive(Debug)]
pub struct StoreRows<'a> {
    store: &'a StoreHandle,
    sql: String,
    params: Vec<Value>,
}

/// The fixed part of the statement; the status list is appended per filter.
///
/// The join to `chunk` supplies the chunk id the order breaks ties on
/// (B-4); the join to `memory` carries the status predicate (B-2). Every
/// join and predicate is on a key the schema indexes.
const SELECT: &str = "SELECT c.chunk_id AS chunk_id, e.memory_id AS memory_id,
        e.chunk_ordinal AS chunk_ordinal, e.dims AS dims, e.vector AS vector
    FROM embedding e
    JOIN chunk c ON c.scope_id = e.scope_id AND c.memory_id = e.memory_id
        AND c.model_revision = e.model_revision AND c.ordinal = e.chunk_ordinal
    JOIN memory m ON m.id = e.memory_id AND m.scope_id = e.scope_id
    WHERE e.scope_id = ?1 AND e.model_revision = ?2 AND e.model_id = ?3
        AND e.dims = ?4 AND e.normalized = ?5
        AND (e.memory_id > ?6 OR (e.memory_id = ?6 AND e.chunk_ordinal > ?7))";

/// The first parameter number the status list takes.
const FIRST_STATUS_PARAM: usize = 8;

impl<'a> StoreRows<'a> {
    /// The source for `query` over `store`.
    #[must_use]
    pub fn new(store: &'a StoreHandle, query: &ScanQuery<'_>) -> Self {
        let labels = query.filter.labels();
        let placeholders = (0..labels.len())
            .map(|offset| format!("?{}", FIRST_STATUS_PARAM + offset))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "{SELECT}\n        AND m.status IN ({placeholders})\n    \
             ORDER BY e.memory_id, e.chunk_ordinal"
        );
        let mut params = vec![
            Value::from(query.scope.as_str()),
            Value::Integer(i64::from(query.model.revision)),
            Value::from(query.model.model_id),
            Value::Integer(i64::from(query.model.dims)),
            Value::from(query.model.normalized),
            Value::Null,
            Value::Null,
        ];
        params.extend(labels.iter().map(|label| Value::from(*label)));
        Self { store, sql, params }
    }
}

/// Index of the keyset parameters `?6` and `?7` within `params`.
const AFTER_MEMORY: usize = 5;
const AFTER_ORDINAL: usize = 6;

impl PageSource for StoreRows<'_> {
    fn page(
        &mut self,
        after: Option<(&str, i64)>,
        size: u32,
    ) -> impl Future<Output = Result<Vec<VectorRow>, Error>> + Send {
        let (memory, ordinal) = after.map_or((String::new(), -1), |(memory, ordinal)| {
            (memory.to_owned(), ordinal)
        });
        if let Some(slot) = self.params.get_mut(AFTER_MEMORY) {
            *slot = Value::from(memory);
        }
        if let Some(slot) = self.params.get_mut(AFTER_ORDINAL) {
            *slot = Value::Integer(ordinal);
        }
        // Keyset paging through `query_paged`: each page starts after the
        // previous page's last key with no offset, so a page costs its own
        // rows and a concurrent write cannot shift a row across a boundary
        // (D-4).
        self.store
            .query_paged(&self.sql, self.params.clone(), Page::new(size))
    }
}

/// Scan `store` for the `k` chunks nearest `query.vector` (B-2).
///
/// # Errors
///
/// [`Error::Validation`] for a zero page size, a query vector whose width
/// differs from the revision's or that cannot be normalized; store errors;
/// [`Error::Integrity`] for a stored row that does not decode.
pub async fn scan(
    store: &StoreHandle,
    query: &ScanQuery<'_>,
    config: ScanConfig,
) -> Result<ScanOutcome, Error> {
    let mut rows = StoreRows::new(store, query);
    scan_rows(&mut rows, query, config).await
}

/// The scan over any [`PageSource`].
///
/// # Errors
///
/// As [`scan`].
pub async fn scan_rows<S: PageSource>(
    source: &mut S,
    query: &ScanQuery<'_>,
    config: ScanConfig,
) -> Result<ScanOutcome, Error> {
    let started = Instant::now();
    if config.page_size == 0 {
        return Err(Error::Validation(
            "a scan page of zero rows never advances".to_owned(),
        ));
    }
    let dims = query.model.dims;
    if query.vector.len() != usize::from(dims) {
        return Err(VectorError::Width {
            expected: dims,
            actual: query.vector.len(),
        }
        .into());
    }
    let mut needle = query.vector.to_vec();
    normalize(&mut needle)?;

    let mut top: TopK<(String, i64)> = TopK::new(query.k);
    let mut buffer = Vec::with_capacity(usize::from(dims));
    let mut cursor: Option<(String, i64)> = None;
    let mut outcome = ScanOutcome {
        candidates: Vec::new(),
        truncated: false,
        scanned: 0,
        pages: 0,
    };
    if query.k == 0 {
        return Ok(outcome);
    }
    loop {
        let after = cursor
            .as_ref()
            .map(|(memory, ordinal)| (memory.as_str(), *ordinal));
        let page = source.page(after, config.page_size).await?;
        if page.is_empty() {
            break;
        }
        outcome.pages = outcome.pages.saturating_add(1);
        let full = u32::try_from(page.len()).is_ok_and(|len| len >= config.page_size);
        if let Some(last) = page.last() {
            cursor = Some((last.memory_id.clone(), last.chunk_ordinal));
        }
        for row in page {
            let row_dims = u16::try_from(row.dims).map_err(|_| {
                Error::Integrity(format!("stored vector width {} is outside u16", row.dims))
            })?;
            if row_dims != dims {
                // The statement selects only rows of the query's width, so a
                // row of another width is a store inconsistency, not a bad
                // request.
                return Err(Error::Integrity(format!(
                    "stored vector of chunk {} has {row_dims} dimensions where its \
                     revision records {dims}",
                    row.chunk_id
                )));
            }
            decode_into(row.vector.as_slice(), row_dims, &mut buffer)?;
            if !query.model.normalized && normalize(&mut buffer).is_err() {
                // A non-normalized model may store a zero vector; it has no
                // direction, so it is similar to nothing (D-5).
                outcome.scanned = outcome.scanned.saturating_add(1);
                continue;
            }
            let score = f64::from(dot(&needle, &buffer));
            outcome.scanned = outcome.scanned.saturating_add(1);
            if top.admits(score, &row.chunk_id) {
                top.offer(score, row.chunk_id, (row.memory_id, row.chunk_ordinal));
            }
        }
        if !full {
            break;
        }
        if started.elapsed() >= config.budget {
            outcome.truncated = true;
            break;
        }
    }
    outcome.candidates = top
        .into_sorted()
        .into_iter()
        .map(|(score, chunk_id, (memory, ordinal))| {
            Ok(Candidate {
                chunk_id,
                memory_id: memory.parse::<MemoryId>().map_err(|error| {
                    Error::Integrity(format!("stored memory id is invalid: {error}"))
                })?,
                ordinal: u32::try_from(ordinal).map_err(|_| {
                    Error::Integrity(format!("chunk ordinal {ordinal} is outside u32"))
                })?,
                score,
            })
        })
        .collect::<Result<_, Error>>()?;
    Ok(outcome)
}

/// The seam an approximate index may fill (B-7).
///
/// [`ScanIndex`] is the only implementation. A second one is added by a
/// later spec, and only with an evaluation run (spec 040) showing the recall
/// it costs and the latency it buys, on a corpus at least ten times the size
/// where the scan first exceeds its budget.
pub trait VectorIndex {
    /// The best `query.k` candidates for `query`.
    fn search(
        &self,
        query: &ScanQuery<'_>,
    ) -> impl Future<Output = Result<ScanOutcome, Error>> + Send;
}

/// The exact scan as a [`VectorIndex`].
#[derive(Clone, Debug)]
pub struct ScanIndex {
    store: StoreHandle,
    config: ScanConfig,
}

impl ScanIndex {
    /// A scan index over `store` with `config`.
    #[must_use]
    pub const fn new(store: StoreHandle, config: ScanConfig) -> Self {
        Self { store, config }
    }

    /// The configuration every search uses.
    #[must_use]
    pub const fn config(&self) -> ScanConfig {
        self.config
    }
}

impl VectorIndex for ScanIndex {
    fn search(
        &self,
        query: &ScanQuery<'_>,
    ) -> impl Future<Output = Result<ScanOutcome, Error>> + Send {
        scan(&self.store, query, self.config)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use aicortex_types::Scope;
    use rahi_types::Sub;

    /// One page of rows of the given widths.
    struct Rows(Vec<u16>);

    impl PageSource for Rows {
        fn page(
            &mut self,
            _after: Option<(&str, i64)>,
            _size: u32,
        ) -> impl Future<Output = Result<Vec<VectorRow>, Error>> + Send {
            let rows = std::mem::take(&mut self.0)
                .into_iter()
                .enumerate()
                .map(|(n, dims)| VectorRow {
                    chunk_id: format!("chunk-{n}"),
                    memory_id: format!("00000000-0000-7000-8000-{n:012x}"),
                    chunk_ordinal: 0,
                    dims: i64::from(dims),
                    vector: Blob::new(vec![0; usize::from(dims) * 4]).unwrap(),
                })
                .collect();
            std::future::ready(Ok(rows))
        }
    }

    #[test]
    fn a_stored_row_of_another_width_is_an_integrity_error() {
        let scope = ScopeId::of(&Scope::personal(Sub::new("width")));
        let filter = Filter::retrievable();
        let needle = [1.0_f32, 0.0];
        let query = ScanQuery {
            scope: &scope,
            model: Comparable {
                model_id: "m",
                revision: 1,
                dims: 2,
                normalized: true,
            },
            vector: &needle,
            k: 1,
            filter: &filter,
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let error = runtime
            .block_on(scan_rows(&mut Rows(vec![3]), &query, ScanConfig::default()))
            .unwrap_err();
        assert!(matches!(error, Error::Integrity(_)), "{error:?}");
        let short = ScanQuery {
            vector: &[1.0],
            ..query
        };
        let error = runtime
            .block_on(scan_rows(&mut Rows(vec![2]), &short, ScanConfig::default()))
            .unwrap_err();
        assert!(matches!(error, Error::Validation(_)), "{error:?}");
    }
}
