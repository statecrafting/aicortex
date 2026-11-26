//! The review queue and the decisions that empty it (spec 023 B-1, B-3, B-4).
//!
//! An item names what is waiting by id: a quarantined memory, a contradiction
//! between two edges, a memory someone asked to have promoted, or a gate
//! override. It holds no content. The queue is filled by reconciliation
//! ([`ReviewQueue::collect`], D-4): the rows that need a person are the
//! quarantined memories and the open contradictions that already exist, so
//! reading them is the one way to find an item that cannot be missed, whether
//! the gate, the embedding worker, or the graph produced it.
//!
//! Every resolution appends one Decision to rahi's ledger and then does what
//! the Decision says, in one transaction, with the item closed in the same
//! transaction. A rejection is the one act whose Decision is 014's: its
//! erasure and the item's close commit together and 014 appends the Decision
//! from the receipt that transaction wrote (D-10). The Decision names ids and
//! reason codes. It never carries a body (constitution XIV).

use std::fmt;
use std::str::FromStr;

use aicortex_store::{
    Authority, Counters, EMBEDDING_NAMESPACE, EmbeddingTarget, Eraser, Erasure, MemoryRepo,
    ScopeId, embedding_processor, stage_active_embedding,
};
use aicortex_types::{AicortexTime, Memory, MemoryId, Scope, Status, TrustClass};
use rahi_ledger::{Decision, DecisionId, DecisionKind, Ledger, Outcome};
use rahi_store::{ProcessingKey, ReceiptKey, Statement, StoreHandle, TxnBuilder, Value, Work};
use rahi_types::{Error, Sub, UnixSeconds};
use serde::Deserialize;
use uuid::Uuid;

use crate::promotion::Promoter;

/// Rows in a page when the caller names no limit.
pub const DEFAULT_PAGE_ROWS: u32 = 25;

/// The most rows one page returns, whatever the caller asks for.
pub const MAX_PAGE_ROWS: u32 = 100;

/// The longest reason text a resolution may carry, in characters.
///
/// The text reaches the ledger, so it is bounded and is a reviewer's words
/// about a choice, not a place to paste what was reviewed.
pub const MAX_REASON_CHARS: usize = 500;

/// The most items one reconciliation pass opens per source (D-4).
const MAX_COLLECT_ROWS: u32 = 200;

/// The most source memories one contradiction shows a reviewer.
const MAX_EVIDENCE_ROWS: u32 = 20;

/// What kind of decision an item waits for (B-1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ReviewKind {
    /// Content whose origin could not be established (013 B-7).
    Quarantine,
    /// Two active edges that disagree (017 B-10).
    Contradiction,
    /// A memory someone wants a client to act on (B-5).
    PromotionRequest,
    /// An operator's override of a gate refusal.
    GateOverride,
}

impl ReviewKind {
    /// The discriminant the `kind` column stores.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Quarantine => "quarantine",
            Self::Contradiction => "contradiction",
            Self::PromotionRequest => "promotion_request",
            Self::GateOverride => "gate_override",
        }
    }

    /// Every kind, in the order the vocabulary is written.
    pub const ALL: [Self; 4] = [
        Self::Quarantine,
        Self::Contradiction,
        Self::PromotionRequest,
        Self::GateOverride,
    ];
}

impl fmt::Display for ReviewKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

impl FromStr for ReviewKind {
    type Err = Error;

    fn from_str(text: &str) -> Result<Self, Error> {
        Self::ALL
            .into_iter()
            .find(|kind| kind.label() == text)
            .ok_or_else(|| {
                Error::Validation(format!(
                    "review kind must be one of quarantine, contradiction, promotion_request, \
                     gate_override; got {text:?}"
                ))
            })
    }
}

/// Where an item is in its life.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ReviewState {
    /// Waiting for a person.
    Open,
    /// A person decided, and a Decision records it.
    Resolved,
    /// What it waited on stopped needing a person without one deciding: the
    /// memory was erased, or the contradiction was closed elsewhere.
    Withdrawn,
}

impl ReviewState {
    /// The discriminant the `state` column stores.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Resolved => "resolved",
            Self::Withdrawn => "withdrawn",
        }
    }

    /// Every state.
    pub const ALL: [Self; 3] = [Self::Open, Self::Resolved, Self::Withdrawn];
}

impl fmt::Display for ReviewState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

impl FromStr for ReviewState {
    type Err = Error;

    fn from_str(text: &str) -> Result<Self, Error> {
        Self::ALL
            .into_iter()
            .find(|state| state.label() == text)
            .ok_or_else(|| {
                Error::Validation(format!(
                    "review state must be one of open, resolved, withdrawn; got {text:?}"
                ))
            })
    }
}

/// One thing waiting for, or decided by, a person (B-1).
///
/// The scope is not a field: every read names it, so an item is always seen
/// through the scope that owns it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewItem {
    /// The item's id.
    pub id: String,
    /// What it waits for.
    pub kind: ReviewKind,
    /// The memory id, or the contradiction id, it is about.
    pub subject: String,
    /// When it was opened.
    pub created: UnixSeconds,
    /// Where it is.
    pub state: ReviewState,
    /// The subject that asked, for a promotion request a person made.
    pub requested_by: Option<Sub>,
    /// The subject that decided, once resolved.
    pub resolved_by: Option<Sub>,
    /// When it was decided.
    pub resolved_at: Option<UnixSeconds>,
    /// The resolution's label, once resolved.
    pub resolution: Option<String>,
    /// The reviewer's reason text, when the resolution took one.
    pub reason: Option<String>,
    /// The ledger Decision that records the resolution.
    pub decision_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ItemRow {
    item_id: String,
    kind: String,
    subject: String,
    created: i64,
    state: String,
    requested_by: Option<String>,
    resolved_by: Option<String>,
    resolved_at: Option<i64>,
    resolution: Option<String>,
    reason: Option<String>,
    decision_id: Option<String>,
}

const ITEM_COLUMNS: &str = "item_id, kind, subject, created, state, requested_by, resolved_by,
    resolved_at, resolution, reason, decision_id";

impl TryFrom<ItemRow> for ReviewItem {
    type Error = Error;

    fn try_from(row: ItemRow) -> Result<Self, Error> {
        let seconds = |value: i64, field: &str| {
            u64::try_from(value).map(UnixSeconds::new).map_err(|_| {
                Error::Integrity(format!(
                    "review item {} has a negative {field}",
                    row.item_id
                ))
            })
        };
        Ok(Self {
            kind: row.kind.parse().map_err(|_| {
                Error::Integrity(format!("review item {} has an unknown kind", row.item_id))
            })?,
            state: row.state.parse().map_err(|_| {
                Error::Integrity(format!("review item {} has an unknown state", row.item_id))
            })?,
            created: seconds(row.created, "created")?,
            resolved_at: row
                .resolved_at
                .map(|at| seconds(at, "resolved_at"))
                .transpose()?,
            requested_by: row.requested_by.map(Sub::new),
            resolved_by: row.resolved_by.map(Sub::new),
            id: row.item_id,
            subject: row.subject,
            resolution: row.resolution,
            reason: row.reason,
            decision_id: row.decision_id,
        })
    }
}

/// Which items a listing returns.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReviewFilter {
    /// Only this kind, when set.
    pub kind: Option<ReviewKind>,
    /// Only this state, when set.
    pub state: Option<ReviewState>,
}

/// One keyset page of the queue, oldest first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewPage {
    /// The items.
    pub items: Vec<ReviewItem>,
    /// The cursor of the next page, absent on the last one.
    pub next: Option<String>,
}

/// What one reconciliation pass did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Collected {
    /// Items opened for quarantined memories and open contradictions.
    pub opened: u64,
    /// Open items whose subject no longer needs a person.
    pub withdrawn: u64,
}

/// The two edges of a contradiction, by id (017 B-10).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContradictionPair {
    /// The lesser edge id.
    pub edge_a: String,
    /// The greater edge id.
    pub edge_b: String,
    /// The shared subject entity.
    pub subject: String,
    /// The shared predicate, from the closed vocabulary (017 D-1).
    pub predicate: String,
}

/// A decision a person takes on one item (B-3, B-4, B-5).
///
/// One variant per choice, so a resolution that does not fit the item's kind
/// is a refusal and not a guess.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Resolution {
    /// Quarantine: let the memory in, at `Assertion` and never higher
    /// (FR-001).
    Admit,
    /// Quarantine: erase the memory through 014 B-7.
    Reject,
    /// Contradiction: keep `survivor`, and supersede the other edge.
    KeepEdge {
        /// The edge that stays active.
        survivor: String,
        /// Why, in the reviewer's words (B-4).
        reason: String,
    },
    /// Contradiction: both edges were true, each in its own period; the
    /// earlier one ends at `boundary` and the later one begins there.
    Disjoint {
        /// The second the periods meet.
        boundary: UnixSeconds,
        /// Why, in the reviewer's words (B-4).
        reason: String,
    },
    /// Promotion request: promote the memory (B-5).
    Approve {
        /// How long before the promotion is reviewed again; the default
        /// applies when absent (D-1 of this spec).
        interval_secs: Option<u64>,
    },
    /// Promotion request: refuse, and leave the memory as it is.
    Decline,
    /// Gate override: note it and close the item.
    Acknowledge,
}

impl Resolution {
    /// The kind of item this resolution answers.
    #[must_use]
    pub const fn kind(&self) -> ReviewKind {
        match self {
            Self::Admit | Self::Reject => ReviewKind::Quarantine,
            Self::KeepEdge { .. } | Self::Disjoint { .. } => ReviewKind::Contradiction,
            Self::Approve { .. } | Self::Decline => ReviewKind::PromotionRequest,
            Self::Acknowledge => ReviewKind::GateOverride,
        }
    }

    /// The label stored on the item and named in the Decision.
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match self {
            Self::Admit => "admit",
            Self::Reject => "reject",
            Self::KeepEdge { .. } => "keep_edge",
            Self::Disjoint { .. } => "disjoint",
            Self::Approve { .. } => "approve",
            Self::Decline => "decline",
            Self::Acknowledge => "acknowledge",
        }
    }

    fn reason(&self) -> Option<&str> {
        match self {
            Self::KeepEdge { reason, .. } | Self::Disjoint { reason, .. } => Some(reason),
            Self::Admit
            | Self::Reject
            | Self::Approve { .. }
            | Self::Decline
            | Self::Acknowledge => None,
        }
    }
}

/// The queue: opening items, reading them, and counting what waits.
#[derive(Clone, Copy, Debug, Default)]
pub struct ReviewQueue;

impl ReviewQueue {
    /// The queue.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    /// Reconcile the queue with the scope (D-4).
    ///
    /// Opens an item for each quarantined memory and each open contradiction
    /// that has none, and withdraws an open item whose subject no longer
    /// needs a person. The two halves are idempotent: the partial unique index
    /// on open items makes a second pass insert nothing, so any number of
    /// readers can reconcile at once. A pass touches at most 200 new rows per
    /// source and says nothing is wrong with the rest: the next pass takes
    /// them.
    ///
    /// # Errors
    ///
    /// The store's error.
    pub async fn collect(
        &self,
        store: &StoreHandle,
        scope: &Scope,
        now: UnixSeconds,
    ) -> Result<Collected, Error> {
        let scope_id = ScopeId::of(scope);
        let at = sql_seconds(now)?;
        let mut txn = TxnBuilder::new();

        let quarantined: Vec<SubjectRow> = store
            .query_consistent(
                "SELECT m.id AS subject FROM memory m
                 WHERE m.scope_id = ?1 AND m.status = 'quarantined'
                   AND NOT EXISTS (
                     SELECT 1 FROM review_item r
                     WHERE r.scope_id = ?1 AND r.kind = 'quarantine'
                       AND r.subject = m.id AND r.state = 'open')
                 ORDER BY m.created, m.id LIMIT ?2",
                vec![
                    Value::from(&scope_id),
                    Value::Integer(i64::from(MAX_COLLECT_ROWS)),
                ],
            )
            .await?;
        let contradictions: Vec<SubjectRow> = store
            .query_consistent(
                "SELECT c.contradiction_id AS subject FROM contradiction c
                 WHERE c.scope_id = ?1 AND c.status = 'open'
                   AND NOT EXISTS (
                     SELECT 1 FROM review_item r
                     WHERE r.scope_id = ?1 AND r.kind = 'contradiction'
                       AND r.subject = c.contradiction_id AND r.state = 'open')
                 ORDER BY c.detected, c.contradiction_id LIMIT ?2",
                vec![
                    Value::from(&scope_id),
                    Value::Integer(i64::from(MAX_COLLECT_ROWS)),
                ],
            )
            .await?;
        let opened = u64::try_from(quarantined.len() + contradictions.len()).unwrap_or(u64::MAX);
        for (kind, rows) in [
            (ReviewKind::Quarantine, quarantined),
            (ReviewKind::Contradiction, contradictions),
        ] {
            for row in rows {
                txn.push(open_statement(&scope_id, kind, &row.subject, None, at));
            }
        }

        for sql in WITHDRAW_SQL {
            txn.push(Statement::with_params(
                sql,
                vec![Value::from(&scope_id), Value::Integer(at)],
            ));
        }
        let statements = txn.into_statements();
        let withdraw_from = statements.len().saturating_sub(WITHDRAW_SQL.len());
        let results = store.txn(statements).await?;
        let withdrawn = results
            .iter()
            .skip(withdraw_from)
            .fold(0_u64, |total, result| {
                total.saturating_add(result.rows_affected)
            });
        Ok(Collected { opened, withdrawn })
    }

    /// One keyset page of the scope's items, oldest first, at most
    /// [`MAX_PAGE_ROWS`].
    ///
    /// `after` is a cursor this function returned. It only positions a read
    /// inside a scope the caller already owns, so it is not signed (as 017
    /// D-9's entity cursor is not).
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] for a limit of zero or a cursor that is not one
    /// this function returned; the store's error.
    pub async fn list(
        &self,
        store: &StoreHandle,
        scope: &Scope,
        filter: ReviewFilter,
        limit: Option<u32>,
        after: Option<&str>,
    ) -> Result<ReviewPage, Error> {
        let limit = limit.unwrap_or(DEFAULT_PAGE_ROWS);
        if limit == 0 {
            return Err(Error::Validation(
                "a review page must ask for at least one row".to_owned(),
            ));
        }
        let limit = limit.min(MAX_PAGE_ROWS);
        let (after_created, after_id) = match after {
            None => (None, None),
            Some(cursor) => {
                let (created, id) = parse_cursor(cursor)?;
                (Some(created), Some(id))
            }
        };
        let rows: Vec<ItemRow> = store
            .query_consistent(
                format!(
                    "SELECT {ITEM_COLUMNS} FROM review_item
                     WHERE scope_id = ?1
                       AND (?2 IS NULL OR kind = ?2)
                       AND (?3 IS NULL OR state = ?3)
                       AND (?4 IS NULL OR created > ?4 OR (created = ?4 AND item_id > ?5))
                     ORDER BY created, item_id LIMIT ?6"
                ),
                vec![
                    Value::from(&ScopeId::of(scope)),
                    Value::from(filter.kind.map(|kind| kind.label().to_owned())),
                    Value::from(filter.state.map(|state| state.label().to_owned())),
                    Value::from(after_created),
                    Value::from(after_id),
                    // One extra row says whether another page follows.
                    Value::Integer(i64::from(limit) + 1),
                ],
            )
            .await?;
        let mut items = rows
            .into_iter()
            .map(ReviewItem::try_from)
            .collect::<Result<Vec<_>, _>>()?;
        let next = if items.len() > usize::try_from(limit).unwrap_or(usize::MAX) {
            items.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
            items
                .last()
                .map(|last| format!("{}.{}", last.created.get(), last.id))
        } else {
            None
        };
        Ok(ReviewPage { items, next })
    }

    /// The item `id` in `scope`, or `None`.
    ///
    /// # Errors
    ///
    /// The store's error.
    pub async fn get(
        &self,
        store: &StoreHandle,
        scope: &Scope,
        id: &str,
    ) -> Result<Option<ReviewItem>, Error> {
        item_in(store, &ScopeId::of(scope), id).await
    }

    /// How many items of `kind` (any when `None`) wait for a person.
    ///
    /// It counts what is stored. Call [`Self::collect`] first when the answer
    /// must include a memory quarantined a moment ago.
    ///
    /// # Errors
    ///
    /// The store's error, or [`Error::Integrity`] for a negative count.
    pub async fn open_count(
        &self,
        store: &StoreHandle,
        scope: &Scope,
        kind: Option<ReviewKind>,
    ) -> Result<u64, Error> {
        let rows: Vec<CountRow> = store
            .query_consistent(
                "SELECT COUNT(*) AS count FROM review_item
                 WHERE scope_id = ?1 AND state = 'open' AND (?2 IS NULL OR kind = ?2)",
                vec![
                    Value::from(&ScopeId::of(scope)),
                    Value::from(kind.map(|kind| kind.label().to_owned())),
                ],
            )
            .await?;
        count_of(rows)
    }

    /// Ask for a promotion (B-1): open a `PromotionRequest` for `memory`.
    ///
    /// Anyone with write access to the scope may ask, an agent included,
    /// because asking decides nothing; only [`Reviewer::resolve`] by an
    /// interactive principal promotes (B-8). A second request while one is
    /// open returns the open one.
    ///
    /// # Errors
    ///
    /// [`Error::NotFound`] when the memory is not in the scope;
    /// [`Error::Conflict`] when it is not active or is already instruction
    /// grade; the store's error.
    pub async fn request_promotion(
        &self,
        store: &StoreHandle,
        scope: &Scope,
        memory: MemoryId,
        requested_by: &Sub,
        now: UnixSeconds,
    ) -> Result<ReviewItem, Error> {
        let found = MemoryRepo::new()
            .get(store, scope, memory)
            .await?
            .ok_or_else(|| Error::NotFound(format!("memory {memory} is not in this scope")))?;
        if found.status != Status::Active {
            return Err(Error::Conflict(format!(
                "memory {memory} is {} and only an active memory can be put forward for promotion",
                found.status.label()
            )));
        }
        if found.trust.is_actionable() {
            return Err(Error::Conflict(format!(
                "memory {memory} is already instruction grade"
            )));
        }
        let scope_id = ScopeId::of(scope);
        let statement = open_statement(
            &scope_id,
            ReviewKind::PromotionRequest,
            &memory.to_string(),
            Some(requested_by),
            sql_seconds(now)?,
        );
        store.txn(vec![statement]).await?;
        let rows: Vec<ItemRow> = store
            .query_consistent(
                format!(
                    "SELECT {ITEM_COLUMNS} FROM review_item
                     WHERE scope_id = ?1 AND kind = 'promotion_request'
                       AND subject = ?2 AND state = 'open'"
                ),
                vec![Value::from(&scope_id), Value::from(memory.to_string())],
            )
            .await?;
        rows.into_iter()
            .next()
            .map(ReviewItem::try_from)
            .transpose()?
            .ok_or_else(|| {
                Error::Integrity(format!(
                    "the promotion request for {memory} did not persist"
                ))
            })
    }

    /// The memories a reviewer needs to see to decide `item`, at most 20.
    ///
    /// A quarantine or a promotion request is about one memory. A
    /// contradiction is decided on the memories its two edges were read from.
    /// Ids only: the routes put the bodies inside 019's envelope.
    ///
    /// # Errors
    ///
    /// [`Error::Integrity`] when the subject is not an id of its kind; the
    /// store's error.
    pub async fn evidence(
        &self,
        store: &StoreHandle,
        scope: &Scope,
        item: &ReviewItem,
    ) -> Result<Vec<MemoryId>, Error> {
        match item.kind {
            ReviewKind::Quarantine | ReviewKind::PromotionRequest | ReviewKind::GateOverride => {
                let id = MemoryId::parse(&item.subject).map_err(|_| {
                    Error::Integrity(format!("review item {} names no memory", item.id))
                })?;
                Ok(vec![id])
            }
            ReviewKind::Contradiction => {
                let rows: Vec<SubjectRow> = store
                    .query_consistent(
                        "SELECT DISTINCT s.memory_id AS subject FROM contradiction c
                         JOIN edge_source s ON s.scope_id = c.scope_id
                          AND (s.edge_id = c.edge_a OR s.edge_id = c.edge_b)
                         WHERE c.scope_id = ?1 AND c.contradiction_id = ?2
                         ORDER BY s.memory_id LIMIT ?3",
                        vec![
                            Value::from(&ScopeId::of(scope)),
                            Value::from(item.subject.as_str()),
                            Value::Integer(i64::from(MAX_EVIDENCE_ROWS)),
                        ],
                    )
                    .await?;
                rows.into_iter()
                    .map(|row| {
                        MemoryId::parse(&row.subject).map_err(|_| {
                            Error::Integrity("an edge source is not a memory id".to_owned())
                        })
                    })
                    .collect()
            }
        }
    }

    /// The two edges of the contradiction `id`, or `None`.
    ///
    /// # Errors
    ///
    /// The store's error.
    pub async fn contradiction(
        &self,
        store: &StoreHandle,
        scope: &Scope,
        id: &str,
    ) -> Result<Option<ContradictionPair>, Error> {
        contradiction_in(store, &ScopeId::of(scope), id)
            .await
            .map(|found| found.map(|(pair, _)| pair))
    }
}

/// Applies the decisions that empty the queue (B-3, B-4, B-5).
#[derive(Clone, Copy)]
pub struct Reviewer<'a> {
    store: &'a StoreHandle,
    ledger: &'a Ledger,
}

impl<'a> Reviewer<'a> {
    /// The reviewer, over the store it decides in and the ledger it records in.
    #[must_use]
    pub const fn new(store: &'a StoreHandle, ledger: &'a Ledger) -> Self {
        Self { store, ledger }
    }

    /// Resolve the open item `id` of `scope` as `by`.
    ///
    /// The caller has already established that `by` is an interactive
    /// principal with the review authority (B-8); this function takes the
    /// subject and never a token, so it cannot be handed an automated
    /// client's credential by mistake. The Decision is appended first and the
    /// act follows in one transaction that also closes the item. A failed act
    /// appends a refusing Decision under a new id and returns the error, so
    /// the chain never shows a success that did not happen.
    ///
    /// # Errors
    ///
    /// [`Error::NotFound`] for an item that is not in the scope;
    /// [`Error::Conflict`] when it is not open, its subject has moved on, or
    /// the act lost a race; [`Error::Validation`] when the resolution does
    /// not answer the item's kind or a reason is missing or too long; the
    /// store's or the ledger's error.
    pub async fn resolve(
        &self,
        scope: &Scope,
        id: &str,
        by: &Sub,
        resolution: &Resolution,
        now: UnixSeconds,
    ) -> Result<ReviewItem, Error> {
        let scope_id = ScopeId::of(scope);
        let item = item_in(self.store, &scope_id, id)
            .await?
            .ok_or_else(|| Error::NotFound(format!("review item {id} is not in this scope")))?;
        if item.state != ReviewState::Open {
            return Err(Error::Conflict(format!(
                "review item {id} is {} and only an open item can be resolved",
                item.state
            )));
        }
        if resolution.kind() != item.kind {
            return Err(Error::Validation(format!(
                "resolution {} answers a {} item, and item {id} is a {} item",
                resolution.label(),
                resolution.kind(),
                item.kind
            )));
        }
        validate_reason(resolution)?;

        let closing = Closing {
            scope_id: &scope_id,
            item_id: &item.id,
            by,
            now,
            resolution: resolution.label(),
            reason: resolution.reason(),
        };
        match resolution {
            Resolution::Admit => self.admit(scope, &item, &closing).await?,
            Resolution::Reject => self.reject(scope, &item, &closing).await?,
            Resolution::KeepEdge { survivor, .. } => {
                self.keep_edge(scope, &item, &closing, survivor).await?;
            }
            Resolution::Disjoint { boundary, .. } => {
                self.disjoint(scope, &item, &closing, *boundary).await?;
            }
            Resolution::Approve { interval_secs } => {
                let memory = MemoryId::parse(&item.subject)
                    .map_err(|_| Error::Integrity(format!("review item {id} names no memory")))?;
                // Promotion closes this item, with the others open for the
                // memory, in its own transaction (B-5).
                Promoter::new(self.store, self.ledger)
                    .promote(scope, memory, by, *interval_secs, now)
                    .await?;
            }
            Resolution::Decline => {
                let decision = decide(
                    self.ledger,
                    "review.decline",
                    &item,
                    scope,
                    by,
                    "a promotion request was declined",
                    resolution,
                )
                .await?;
                close_only(self.store, &closing, &decision).await?;
            }
            Resolution::Acknowledge => {
                let decision = decide(
                    self.ledger,
                    "review.acknowledge",
                    &item,
                    scope,
                    by,
                    "a gate override was acknowledged",
                    resolution,
                )
                .await?;
                close_only(self.store, &closing, &decision).await?;
            }
        }
        item_in(self.store, &scope_id, id).await?.ok_or_else(|| {
            Error::Integrity(format!("review item {id} disappeared while being resolved"))
        })
    }

    /// Quarantine, admit (B-3, FR-001).
    async fn admit(
        &self,
        scope: &Scope,
        item: &ReviewItem,
        closing: &Closing<'_>,
    ) -> Result<(), Error> {
        let id = subject_memory(item)?;
        let (mut memory, expected) = load(self.store, scope, id)
            .await?
            .ok_or_else(|| Error::NotFound(format!("memory {id} is not in this scope")))?;
        if memory.status != Status::Quarantined {
            return Err(Error::Conflict(format!(
                "memory {id} is {} and only a quarantined memory can be admitted",
                memory.status.label()
            )));
        }
        memory.status = Status::Active;
        // Never higher than an assertion: an admitted memory has an origin
        // nobody could establish, so it is somebody's claim (FR-001).
        memory.trust = TrustClass::Assertion;
        memory.updated = closing.now;
        let target = EmbeddingTarget::observe(self.store).await?;

        let decision = decide(
            self.ledger,
            "review.admit",
            item,
            scope,
            closing.by,
            "a quarantined memory was admitted at assertion grade",
            &Resolution::Admit,
        )
        .await?;
        let mut txn = TxnBuilder::new();
        txn.push(memory_update(
            closing.scope_id,
            &memory,
            Status::Quarantined.label(),
            &expected,
        )?);
        Counters::adjust(
            &mut txn,
            closing.scope_id,
            memory.kind,
            &Status::Quarantined,
            -1,
        );
        Counters::adjust(&mut txn, closing.scope_id, memory.kind, &Status::Active, 1);
        if let Some(model) = target.model() {
            // The embedding worker holds a quarantined memory's work as a
            // dead letter; admission puts it back (015 B-1).
            stage_active_embedding(&mut txn, closing.scope_id.as_str(), id, model, closing.now)?;
            let receipt = ReceiptKey::new(
                closing.scope_id.as_str(),
                EMBEDDING_NAMESPACE,
                id.to_string(),
            )?;
            let key = ProcessingKey::new(
                receipt,
                model.revision,
                embedding_processor(model.revision),
                &model.model_id,
            )?;
            Work::requeue(&mut txn, &key, closing.now);
        }
        txn.push(closing.statement(&decision)?);
        commit_or_refuse(
            self.store,
            self.ledger,
            scope,
            item,
            closing,
            txn,
            "review.admit_failed",
        )
        .await
    }

    /// Quarantine, reject: erasure, through 014 B-7 (B-3).
    async fn reject(
        &self,
        scope: &Scope,
        item: &ReviewItem,
        closing: &Closing<'_>,
    ) -> Result<(), Error> {
        let id = subject_memory(item)?;
        let memory = MemoryRepo::new()
            .get(self.store, scope, id)
            .await?
            .ok_or_else(|| Error::NotFound(format!("memory {id} is not in this scope")))?;
        if memory.status != Status::Quarantined {
            return Err(Error::Conflict(format!(
                "memory {id} is {} and only a quarantined memory can be rejected",
                memory.status.label()
            )));
        }
        let authority = Authority::of(closing.by.clone()).because("review_reject");
        let eraser = Eraser::new();
        let prepared = eraser
            .prepare_erase(self.store, scope, id, false, &authority, closing.now)
            .await?;
        // The erasure and the item's close commit together (D-10): a failure
        // of either leaves the memory quarantined and the item open, so the
        // reconciliation never withdraws an item for a memory that a
        // rejection erased.
        let mut txn = TxnBuilder::new();
        let staged = Eraser::stage_erase(&mut txn, prepared)?;
        txn.push(closing.statement(staged.decision.as_str())?);
        self.store.txn(txn.into_statements()).await?;
        // The erasure Decision is appended after its transaction commits, by
        // 014's rule, from the durable receipt that transaction wrote; this
        // call delivers it, and a failure here is retried by repeating it.
        eraser
            .erase(
                self.store,
                self.ledger,
                &Erasure::new(scope, id, &authority, closing.now),
            )
            .await?;
        Ok(())
    }

    /// Contradiction, keep one edge (B-4).
    async fn keep_edge(
        &self,
        scope: &Scope,
        item: &ReviewItem,
        closing: &Closing<'_>,
        survivor: &str,
    ) -> Result<(), Error> {
        let (pair, _) = contradiction_edges(self.store, closing.scope_id, item).await?;
        let loser = if survivor == pair.edge_a {
            &pair.edge_b
        } else if survivor == pair.edge_b {
            &pair.edge_a
        } else {
            return Err(Error::Validation(format!(
                "the surviving edge must be {} or {} of contradiction {}; got {survivor}",
                pair.edge_a, pair.edge_b, item.subject
            )));
        };
        let at = sql_seconds(closing.now)?;
        let decision = decide(
            self.ledger,
            "review.contradiction",
            item,
            scope,
            closing.by,
            "a contradiction was resolved by keeping one edge",
            &Resolution::KeepEdge {
                survivor: survivor.to_owned(),
                reason: closing.reason.unwrap_or_default().to_owned(),
            },
        )
        .await?;
        let mut txn = TxnBuilder::new();
        // The loser stops holding when the reviewer decided, never before it
        // began, and never later than an end it already had (D-7); nothing is
        // deleted (017 FR-005). An edge no longer active when this commits
        // writes NULL into the NOT NULL status, aborting the whole
        // transaction, so the recorded Decision never closes an item whose
        // act did not happen (FR-003); `commit_or_refuse` then records the
        // refusal.
        txn.push(Statement::with_params(
            "UPDATE edge SET
                 status = CASE WHEN status = 'active' THEN 'superseded' ELSE NULL END,
                 valid_until = MIN(COALESCE(valid_until, MAX(valid_from, ?3)), MAX(valid_from, ?3))
             WHERE scope_id = ?1 AND edge_id = ?2",
            vec![
                Value::from(closing.scope_id),
                Value::from(loser.as_str()),
                Value::Integer(at),
            ],
        ));
        txn.push(close_contradiction(closing.scope_id, &item.subject));
        txn.push(closing.statement(&decision)?);
        commit_or_refuse(
            self.store,
            self.ledger,
            scope,
            item,
            closing,
            txn,
            "review.contradiction_failed",
        )
        .await
    }

    /// Contradiction, both were true in their own period (B-4).
    async fn disjoint(
        &self,
        scope: &Scope,
        item: &ReviewItem,
        closing: &Closing<'_>,
        boundary: UnixSeconds,
    ) -> Result<(), Error> {
        let (pair, edges) = contradiction_edges(self.store, closing.scope_id, item).await?;
        let [first, second] = order_by_start(edges, &pair)?;
        let meet = sql_seconds(boundary)?;
        let latest_start = first.valid_from.max(second.valid_from);
        let earliest_end = first
            .valid_until
            .unwrap_or(i64::MAX)
            .min(second.valid_until.unwrap_or(i64::MAX));
        if meet <= first.valid_from || meet < latest_start || meet > earliest_end {
            return Err(Error::Validation(format!(
                "the boundary must fall after the earlier edge begins ({}) and inside the \
                 period both edges claim, from {latest_start} to {}; got {meet}",
                first.valid_from,
                if earliest_end == i64::MAX {
                    "no end".to_owned()
                } else {
                    earliest_end.to_string()
                },
            )));
        }
        let decision = decide(
            self.ledger,
            "review.contradiction",
            item,
            scope,
            closing.by,
            "a contradiction was resolved as two disjoint periods",
            &Resolution::Disjoint {
                boundary,
                reason: closing.reason.unwrap_or_default().to_owned(),
            },
        )
        .await?;
        let mut txn = TxnBuilder::new();
        // As in `keep_edge`: an edge no longer active aborts the transaction.
        txn.push(Statement::with_params(
            "UPDATE edge SET valid_until = ?3,
                 status = CASE WHEN status = 'active' THEN status ELSE NULL END
             WHERE scope_id = ?1 AND edge_id = ?2",
            vec![
                Value::from(closing.scope_id),
                Value::from(first.edge_id.as_str()),
                Value::Integer(meet),
            ],
        ));
        txn.push(Statement::with_params(
            "UPDATE edge SET valid_from = ?3,
                 status = CASE WHEN status = 'active' THEN status ELSE NULL END
             WHERE scope_id = ?1 AND edge_id = ?2",
            vec![
                Value::from(closing.scope_id),
                Value::from(second.edge_id.as_str()),
                Value::Integer(meet),
            ],
        ));
        txn.push(close_contradiction(closing.scope_id, &item.subject));
        txn.push(closing.statement(&decision)?);
        commit_or_refuse(
            self.store,
            self.ledger,
            scope,
            item,
            closing,
            txn,
            "review.contradiction_failed",
        )
        .await
    }
}

/// What closing an item writes.
pub(crate) struct Closing<'a> {
    pub(crate) scope_id: &'a ScopeId,
    pub(crate) item_id: &'a str,
    pub(crate) by: &'a Sub,
    pub(crate) now: UnixSeconds,
    pub(crate) resolution: &'a str,
    pub(crate) reason: Option<&'a str>,
}

impl Closing<'_> {
    /// The statement that resolves the item, naming the Decision.
    fn statement(&self, decision: &str) -> Result<Statement, Error> {
        Ok(Statement::with_params(
            "UPDATE review_item
             SET state = 'resolved', resolved_by = ?3, resolved_at = ?4,
                 resolution = ?5, reason = ?6, decision_id = ?7
             WHERE scope_id = ?1 AND item_id = ?2 AND state = 'open'",
            vec![
                Value::from(self.scope_id),
                Value::from(self.item_id),
                Value::from(self.by.as_str()),
                Value::Integer(sql_seconds(self.now)?),
                Value::from(self.resolution),
                Value::from(self.reason.map(str::to_owned)),
                Value::from(decision),
            ],
        ))
    }
}

#[derive(Debug, Deserialize)]
struct SubjectRow {
    subject: String,
}

#[derive(Debug, Deserialize)]
struct CountRow {
    count: i64,
}

#[derive(Debug, Deserialize)]
struct PairRow {
    edge_a: String,
    edge_b: String,
    subject: String,
    predicate: String,
    status: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct EdgeRow {
    pub(crate) edge_id: String,
    pub(crate) valid_from: i64,
    pub(crate) valid_until: Option<i64>,
    pub(crate) status: String,
}

/// Withdraw open items whose subject no longer needs a person (D-4). Each
/// statement binds the scope as `?1` and the time as `?2`; the time is
/// unused by the first three and bound so all four share one parameter list.
const WITHDRAW_SQL: [&str; 3] = [
    "UPDATE review_item SET state = 'withdrawn', resolved_at = ?2
     WHERE scope_id = ?1 AND state = 'open' AND kind = 'quarantine'
       AND NOT EXISTS (SELECT 1 FROM memory m WHERE m.scope_id = ?1
                       AND m.id = review_item.subject AND m.status = 'quarantined')",
    "UPDATE review_item SET state = 'withdrawn', resolved_at = ?2
     WHERE scope_id = ?1 AND state = 'open' AND kind = 'contradiction'
       AND NOT EXISTS (SELECT 1 FROM contradiction c WHERE c.scope_id = ?1
                       AND c.contradiction_id = review_item.subject AND c.status = 'open')",
    "UPDATE review_item SET state = 'withdrawn', resolved_at = ?2
     WHERE scope_id = ?1 AND state = 'open' AND kind = 'promotion_request'
       AND NOT EXISTS (SELECT 1 FROM memory m WHERE m.scope_id = ?1
                       AND m.id = review_item.subject AND m.status = 'active'
                       AND m.trust <> 'instruction')",
];

fn open_statement(
    scope_id: &ScopeId,
    kind: ReviewKind,
    subject: &str,
    requested_by: Option<&Sub>,
    at: i64,
) -> Statement {
    Statement::with_params(
        "INSERT INTO review_item
           (item_id, scope_id, kind, subject, created, state, requested_by)
         VALUES (?1, ?2, ?3, ?4, ?5, 'open', ?6)
         ON CONFLICT DO NOTHING",
        vec![
            Value::from(format!("rvw-{}", Uuid::now_v7().simple())),
            Value::from(scope_id),
            Value::from(kind.label()),
            Value::from(subject),
            Value::Integer(at),
            Value::from(requested_by.map(|sub| sub.as_str().to_owned())),
        ],
    )
}

fn close_contradiction(scope_id: &ScopeId, id: &str) -> Statement {
    Statement::with_params(
        "UPDATE contradiction SET status = 'resolved'
         WHERE scope_id = ?1 AND contradiction_id = ?2 AND status = 'open'",
        vec![Value::from(scope_id), Value::from(id)],
    )
}

/// The statement that writes a changed memory record, guarded by the record
/// the caller read and the status it expects (the supersession pattern of
/// 014 B-3): a lost race makes the `record` column `NULL`, which the schema
/// refuses, and the transaction rolls back as a conflict.
pub(crate) fn memory_update(
    scope_id: &ScopeId,
    memory: &Memory,
    expected_status: &str,
    expected_record: &str,
) -> Result<Statement, Error> {
    Ok(Statement::with_params(
        "UPDATE memory SET status = ?1, trust = ?2,
             record = CASE WHEN record = ?3 AND status = ?4 THEN ?5 ELSE NULL END,
             updated = ?6
         WHERE scope_id = ?7 AND id = ?8",
        vec![
            Value::from(memory.status.label()),
            Value::from(memory.trust.label()),
            Value::from(expected_record),
            Value::from(expected_status),
            Value::from(encode(memory)?),
            Value::Integer(sql_seconds(memory.updated)?),
            Value::from(scope_id),
            Value::from(memory.id.to_string()),
        ],
    ))
}

#[derive(Debug, Deserialize)]
struct RecordRow {
    record: String,
}

/// The memory `id` of `scope` and the exact record text it was read from.
///
/// The text is what [`memory_update`] compares the row against, so a change
/// commits only over the record that was read: a record another writer
/// formatted differently still matches itself, where a re-encoding of the
/// decoded memory would not.
pub(crate) async fn load(
    store: &StoreHandle,
    scope: &Scope,
    id: MemoryId,
) -> Result<Option<(Memory, String)>, Error> {
    let rows: Vec<RecordRow> = store
        .query_consistent(
            "SELECT record FROM memory WHERE scope_id = ?1 AND id = ?2",
            vec![
                Value::from(&ScopeId::of(scope)),
                Value::from(id.to_string()),
            ],
        )
        .await?;
    rows.into_iter()
        .next()
        .map(|row| {
            let memory = serde_json::from_str(&row.record).map_err(|error| {
                Error::Integrity(format!(
                    "memory {id} holds a record that does not decode: {error}"
                ))
            })?;
            Ok((memory, row.record))
        })
        .transpose()
}

/// The stored form of a memory record (012 D-3): written to the `record`
/// column, never sent out. Bodies leave only through 019's envelope.
fn encode(record: &Memory) -> Result<String, Error> {
    serde_json::to_string(record).map_err(|error| {
        Error::Validation(format!("memory {} does not serialize: {error}", record.id))
    })
}

pub(crate) fn sql_seconds(at: AicortexTime) -> Result<i64, Error> {
    i64::try_from(at.get()).map_err(|_| {
        Error::Validation(format!(
            "time {} is beyond what the store can hold",
            at.get()
        ))
    })
}

fn subject_memory(item: &ReviewItem) -> Result<MemoryId, Error> {
    MemoryId::parse(&item.subject)
        .map_err(|_| Error::Integrity(format!("review item {} names no memory", item.id)))
}

fn validate_reason(resolution: &Resolution) -> Result<(), Error> {
    let Some(reason) = resolution.reason() else {
        return Ok(());
    };
    let length = reason.chars().count();
    if reason.trim().is_empty() || length > MAX_REASON_CHARS {
        return Err(Error::Validation(format!(
            "a {} resolution needs a reason of 1 to {MAX_REASON_CHARS} characters; got {length}",
            resolution.label()
        )));
    }
    if reason.chars().any(char::is_control) {
        return Err(Error::Validation(
            "a reason must not carry control characters".to_owned(),
        ));
    }
    Ok(())
}

fn parse_cursor(cursor: &str) -> Result<(i64, String), Error> {
    cursor
        .split_once('.')
        .and_then(|(created, id)| {
            let created = created
                .parse::<i64>()
                .ok()
                .filter(|created| *created >= 0)?;
            (id.starts_with("rvw-") && id.len() <= 64).then(|| (created, id.to_owned()))
        })
        .ok_or_else(|| {
            Error::Validation("cursor must be the value a previous review page returned".to_owned())
        })
}

fn count_of(rows: Vec<CountRow>) -> Result<u64, Error> {
    let row = rows
        .into_iter()
        .next()
        .ok_or_else(|| Error::Integrity("a COUNT returned no row".to_owned()))?;
    u64::try_from(row.count).map_err(|_| Error::Integrity("a count is negative".to_owned()))
}

async fn item_in(
    store: &StoreHandle,
    scope_id: &ScopeId,
    id: &str,
) -> Result<Option<ReviewItem>, Error> {
    let rows: Vec<ItemRow> = store
        .query_consistent(
            format!("SELECT {ITEM_COLUMNS} FROM review_item WHERE scope_id = ?1 AND item_id = ?2"),
            vec![Value::from(scope_id), Value::from(id)],
        )
        .await?;
    rows.into_iter()
        .next()
        .map(ReviewItem::try_from)
        .transpose()
}

async fn contradiction_in(
    store: &StoreHandle,
    scope_id: &ScopeId,
    id: &str,
) -> Result<Option<(ContradictionPair, String)>, Error> {
    let rows: Vec<PairRow> = store
        .query_consistent(
            "SELECT edge_a, edge_b, subject, predicate, status FROM contradiction
             WHERE scope_id = ?1 AND contradiction_id = ?2",
            vec![Value::from(scope_id), Value::from(id)],
        )
        .await?;
    Ok(rows.into_iter().next().map(|row| {
        (
            ContradictionPair {
                edge_a: row.edge_a,
                edge_b: row.edge_b,
                subject: row.subject,
                predicate: row.predicate,
            },
            row.status,
        )
    }))
}

/// The contradiction an item is about and its two edges, both active.
async fn contradiction_edges(
    store: &StoreHandle,
    scope_id: &ScopeId,
    item: &ReviewItem,
) -> Result<(ContradictionPair, Vec<EdgeRow>), Error> {
    let (pair, status) = contradiction_in(store, scope_id, &item.subject)
        .await?
        .ok_or_else(|| {
            Error::NotFound(format!(
                "contradiction {} is not in this scope",
                item.subject
            ))
        })?;
    if status != "open" {
        return Err(Error::Conflict(format!(
            "contradiction {} is {status} and only an open one can be resolved",
            item.subject
        )));
    }
    let edges: Vec<EdgeRow> = store
        .query_consistent(
            "SELECT edge_id, valid_from, valid_until, status FROM edge
             WHERE scope_id = ?1 AND edge_id IN (?2, ?3) ORDER BY edge_id",
            vec![
                Value::from(scope_id),
                Value::from(pair.edge_a.as_str()),
                Value::from(pair.edge_b.as_str()),
            ],
        )
        .await?;
    if edges.len() != 2 || edges.iter().any(|edge| edge.status != "active") {
        return Err(Error::Conflict(format!(
            "both edges of contradiction {} must be active to resolve it",
            item.subject
        )));
    }
    Ok((pair, edges))
}

/// The two edges earlier first, ties by id.
fn order_by_start(
    mut edges: Vec<EdgeRow>,
    pair: &ContradictionPair,
) -> Result<[EdgeRow; 2], Error> {
    edges.sort_by(|a, b| (a.valid_from, &a.edge_id).cmp(&(b.valid_from, &b.edge_id)));
    let mut edges = edges.into_iter();
    match (edges.next(), edges.next(), edges.next()) {
        (Some(first), Some(second), None) => Ok([first, second]),
        _ => Err(Error::Integrity(format!(
            "contradiction between {} and {} does not have two edges",
            pair.edge_a, pair.edge_b
        ))),
    }
}

/// Append the Decision for a resolution and return its id (B-3, B-4).
///
/// The id is derived from the item, so a retry after a failed act appends the
/// same Decision again, which the ledger accepts as the same record and a
/// different resolution of the same item refuses as a conflict.
pub(crate) async fn decide(
    ledger: &Ledger,
    kind: &str,
    item: &ReviewItem,
    scope: &Scope,
    by: &Sub,
    summary: &str,
    resolution: &Resolution,
) -> Result<String, Error> {
    let id = format!("{kind}-{}", item.id);
    let decision = Decision::new(
        DecisionId::new(id.clone()),
        DecisionKind::new(kind),
        by.clone(),
        Outcome::Allow,
        summary,
    )
    .with_payload(serde_json::json!({
        "scope": scope.to_string(),
        "item": item.id,
        "review_kind": item.kind.label(),
        "subject": item.subject,
        "resolution": resolution.label(),
        "reason": resolution.reason(),
    }));
    ledger.append(decision).await?;
    Ok(id)
}

/// Commit the act, or record that it failed and return why (B-3).
async fn commit_or_refuse(
    store: &StoreHandle,
    ledger: &Ledger,
    scope: &Scope,
    item: &ReviewItem,
    closing: &Closing<'_>,
    txn: TxnBuilder,
    failed_kind: &str,
) -> Result<(), Error> {
    match store.txn(txn.into_statements()).await {
        Ok(_) => Ok(()),
        Err(error) => {
            // Best effort: the original error is the one the caller needs.
            let refusal = Decision::new(
                DecisionId::new(format!(
                    "{failed_kind}-{}-{}",
                    item.id,
                    Uuid::now_v7().simple()
                )),
                DecisionKind::new(failed_kind),
                closing.by.clone(),
                Outcome::Deny,
                "a review decision was recorded but its act did not commit",
            )
            .with_payload(serde_json::json!({
                "scope": scope.to_string(),
                "item": item.id,
                "resolution": closing.resolution,
            }));
            let _ = ledger.append(refusal).await;
            Err(error)
        }
    }
}

/// Close an item whose resolution has no act beyond its Decision.
async fn close_only(
    store: &StoreHandle,
    closing: &Closing<'_>,
    decision: &str,
) -> Result<(), Error> {
    store.txn(vec![closing.statement(decision)?]).await?;
    Ok(())
}
