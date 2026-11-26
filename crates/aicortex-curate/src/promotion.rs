//! Promotion, demotion, and the lapse of a promotion (spec 023 B-5, B-6, B-7).
//!
//! Instruction grade is the one trust class a client may act on, and the only
//! way to it is [`aicortex_types::TrustClass::instruction`], which demands a
//! [`aicortex_types::Promotion`]: a human's [`Sub`] and the id of a Decision
//! in rahi's ledger (011 B-5). This module is where both are produced. The
//! Decision is appended first, its id becomes part of the memory's trust, and
//! the memory is written in one transaction with the promotion's row and the
//! closing of any request waiting on it.
//!
//! The functions take a [`Sub`], never a credential, and nothing in the crate
//! reaches them from an automated client: the interactive-principal check is
//! the route's (B-8), and a [`Sub`] is not something an agent has (011 B-7).

use aicortex_store::ScopeId;
use aicortex_types::{DecisionRef, Memory, MemoryId, Promotion, Scope, Status, TrustClass};
use rahi_ledger::{Decision, DecisionId, DecisionKind, Ledger, Outcome};
use rahi_store::{Statement, StoreHandle, TxnBuilder, Value};
use rahi_types::{Error, Sub, UnixSeconds};
use serde::Deserialize;
use uuid::Uuid;

use crate::review::{ReviewKind, load, memory_update, sql_seconds};

/// How long a promotion stands before it is reviewed again: one year (D-1 of
/// this spec).
pub const DEFAULT_REVIEW_INTERVAL_SECS: u64 = 365 * 24 * 60 * 60;

/// The shortest review interval a promotion may carry: one hour.
///
/// Below it a promotion would lapse before a client could act on it.
pub const MIN_REVIEW_INTERVAL_SECS: u64 = 60 * 60;

/// The longest review interval a promotion may carry: ten years.
///
/// There is no interval of "never". A standing instruction nobody has
/// reconfirmed is not a standing instruction (B-7).
pub const MAX_REVIEW_INTERVAL_SECS: u64 = 10 * 365 * 24 * 60 * 60;

/// The most lapsed promotions one sweep takes.
pub const MAX_EXPIRY_BATCH: u32 = 100;

/// The Decision kind of a promotion.
const KIND_PROMOTE: &str = "memory.promote";
/// The Decision kind of a demotion.
const KIND_DEMOTE: &str = "memory.demote";
/// The Decision kind of a lapse.
const KIND_LAPSE: &str = "memory.promotion_lapsed";

/// A memory raised to instruction grade.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Promoted {
    /// The memory.
    pub memory: MemoryId,
    /// The Decision that records the act and is part of the memory's trust.
    pub decision: String,
    /// When the promotion is next reviewed.
    pub review_due: UnixSeconds,
}

/// A memory returned to assertion grade by a person.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Demoted {
    /// The memory.
    pub memory: MemoryId,
    /// The Decision that records the act.
    pub decision: String,
}

/// A promotion that lapsed and was returned to assertion grade (B-7).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lapsed {
    /// The memory.
    pub memory: MemoryId,
    /// The Decision that records the lapse.
    pub decision: String,
}

#[derive(Debug, Deserialize)]
struct DueRow {
    memory_id: String,
    decision_id: String,
    promoted_by: String,
}

/// The only code that writes `Instruction` as a trust class.
#[derive(Clone, Copy)]
pub struct Promoter<'a> {
    store: &'a StoreHandle,
    ledger: &'a Ledger,
}

impl<'a> Promoter<'a> {
    /// The promoter, over the store it writes and the ledger it records in.
    #[must_use]
    pub const fn new(store: &'a StoreHandle, ledger: &'a Ledger) -> Self {
        Self { store, ledger }
    }

    /// Promote `memory` on the authority of `by` (B-5, FR-002).
    ///
    /// Appends exactly one Decision, then writes the memory's trust as
    /// `Instruction` naming it, records the promotion's review date, and
    /// resolves every promotion request open for the memory. `interval_secs`
    /// is the review interval; the default applies when it is absent.
    ///
    /// If the write loses a race after the Decision is appended, a refusing
    /// Decision is appended under a new id and the error is returned, so the
    /// chain does not show a promotion that did not happen (FR-003's rule
    /// applied to a lost race).
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] for an interval outside the bounds;
    /// [`Error::NotFound`] when the memory is not in the scope;
    /// [`Error::Conflict`] when it is not active, is already instruction
    /// grade, or changed while the Decision was being appended; the store's
    /// or the ledger's error.
    pub async fn promote(
        &self,
        scope: &Scope,
        memory: MemoryId,
        by: &Sub,
        interval_secs: Option<u64>,
        now: UnixSeconds,
    ) -> Result<Promoted, Error> {
        let interval = interval_secs.unwrap_or(DEFAULT_REVIEW_INTERVAL_SECS);
        if !(MIN_REVIEW_INTERVAL_SECS..=MAX_REVIEW_INTERVAL_SECS).contains(&interval) {
            return Err(Error::Validation(format!(
                "a promotion's review interval must be {MIN_REVIEW_INTERVAL_SECS} to \
                 {MAX_REVIEW_INTERVAL_SECS} seconds; got {interval}"
            )));
        }
        let review_due = UnixSeconds::new(now.get().checked_add(interval).ok_or_else(|| {
            Error::Validation("the review date is beyond what a time can hold".to_owned())
        })?);
        let scope_id = ScopeId::of(scope);
        let (mut found, expected) = existing(self.store, scope, memory).await?;
        if found.status != Status::Active {
            return Err(Error::Conflict(format!(
                "memory {memory} is {} and only an active memory can be promoted",
                found.status.label()
            )));
        }
        if found.trust.is_actionable() {
            return Err(Error::Conflict(format!(
                "memory {memory} is already instruction grade; demote it before promoting again"
            )));
        }

        let decision_id = format!("{KIND_PROMOTE}-{memory}-{}", Uuid::now_v7().simple());
        let reference = DecisionRef::new(decision_id.clone())
            .map_err(|error| Error::Validation(error.to_string()))?;
        self.ledger
            .append(
                Decision::new(
                    DecisionId::new(decision_id.clone()),
                    DecisionKind::new(KIND_PROMOTE),
                    by.clone(),
                    Outcome::Allow,
                    "a memory was promoted to instruction grade by a person",
                )
                .with_payload(serde_json::json!({
                    "scope": scope.to_string(),
                    "memory_id": memory.to_string(),
                    "promoted_by": by.as_str(),
                    "review_due": review_due.get(),
                })),
            )
            .await?;

        found.trust = TrustClass::instruction(Promotion::new(by.clone(), reference));
        found.updated = now;
        let mut txn = TxnBuilder::new();
        txn.push(memory_update(
            &scope_id,
            &found,
            Status::Active.label(),
            &expected,
        )?);
        txn.push(Statement::with_params(
            "INSERT INTO promotion
               (scope_id, memory_id, decision_id, promoted_by, promoted_at, review_due)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT (scope_id, memory_id) DO UPDATE SET
               decision_id = excluded.decision_id, promoted_by = excluded.promoted_by,
               promoted_at = excluded.promoted_at, review_due = excluded.review_due",
            vec![
                Value::from(&scope_id),
                Value::from(memory.to_string()),
                Value::from(decision_id.as_str()),
                Value::from(by.as_str()),
                Value::Integer(sql_seconds(now)?),
                Value::Integer(sql_seconds(review_due)?),
            ],
        ));
        txn.push(Statement::with_params(
            "UPDATE review_item
             SET state = 'resolved', resolved_by = ?3, resolved_at = ?4,
                 resolution = 'approve', decision_id = ?5
             WHERE scope_id = ?1 AND kind = ?6 AND subject = ?2 AND state = 'open'",
            vec![
                Value::from(&scope_id),
                Value::from(memory.to_string()),
                Value::from(by.as_str()),
                Value::Integer(sql_seconds(now)?),
                Value::from(decision_id.as_str()),
                Value::from(ReviewKind::PromotionRequest.label()),
            ],
        ));
        if let Err(error) = self.store.txn(txn.into_statements()).await {
            refuse(self.ledger, KIND_PROMOTE, scope, memory, by).await;
            return Err(error);
        }
        Ok(Promoted {
            memory,
            decision: decision_id,
            review_due,
        })
    }

    /// Return `memory` to assertion grade on the authority of `by` (B-6).
    ///
    /// # Errors
    ///
    /// [`Error::NotFound`] when the memory is not in the scope;
    /// [`Error::Conflict`] when it is not instruction grade or changed while
    /// the Decision was being appended; the store's or the ledger's error.
    pub async fn demote(
        &self,
        scope: &Scope,
        memory: MemoryId,
        by: &Sub,
        now: UnixSeconds,
    ) -> Result<Demoted, Error> {
        let (mut found, expected) = existing(self.store, scope, memory).await?;
        if found.status == Status::Erased || !found.trust.is_actionable() {
            return Err(Error::Conflict(format!(
                "memory {memory} is not instruction grade, so there is nothing to demote"
            )));
        }
        let scope_id = ScopeId::of(scope);
        let decision_id = format!("{KIND_DEMOTE}-{memory}-{}", Uuid::now_v7().simple());
        self.ledger
            .append(
                Decision::new(
                    DecisionId::new(decision_id.clone()),
                    DecisionKind::new(KIND_DEMOTE),
                    by.clone(),
                    Outcome::Allow,
                    "a memory was returned to assertion grade by a person",
                )
                .with_payload(serde_json::json!({
                    "scope": scope.to_string(),
                    "memory_id": memory.to_string(),
                    "demoted_by": by.as_str(),
                })),
            )
            .await?;
        let status = found.status.label();
        found.trust = TrustClass::Assertion;
        found.updated = now;
        let mut txn = TxnBuilder::new();
        txn.push(memory_update(&scope_id, &found, status, &expected)?);
        txn.push(delete_promotion(&scope_id, memory));
        if let Err(error) = self.store.txn(txn.into_statements()).await {
            refuse(self.ledger, KIND_DEMOTE, scope, memory, by).await;
            return Err(error);
        }
        Ok(Demoted {
            memory,
            decision: decision_id,
        })
    }

    /// Return every promotion of `scope` due for review at `now` to
    /// assertion grade, and open a request to reconfirm each (B-7, FR-004).
    ///
    /// The clock is the caller's, so a test sets it and a deployment's worker
    /// passes the real one. A pass takes at most [`MAX_EXPIRY_BATCH`]; the
    /// next pass takes the rest. A promotion whose memory is gone or no
    /// longer instruction grade only loses its row. The Decision's id is
    /// derived from the promotion's, so a pass interrupted after appending
    /// repeats the same record and not a second one. The Decision names the
    /// subject who promoted, because a lapse is a consequence of that act and
    /// the ledger records an actor for every Decision (D-5 of this spec).
    ///
    /// # Errors
    ///
    /// The store's or the ledger's error. A memory that changed under the
    /// sweep is skipped and is picked up by the next pass.
    pub async fn expire_due(&self, scope: &Scope, now: UnixSeconds) -> Result<Vec<Lapsed>, Error> {
        let scope_id = ScopeId::of(scope);
        let due: Vec<DueRow> = self
            .store
            .query_consistent(
                "SELECT memory_id, decision_id, promoted_by FROM promotion
                 WHERE scope_id = ?1 AND review_due <= ?2
                 ORDER BY review_due, memory_id LIMIT ?3",
                vec![
                    Value::from(&scope_id),
                    Value::Integer(sql_seconds(now)?),
                    Value::Integer(i64::from(MAX_EXPIRY_BATCH)),
                ],
            )
            .await?;
        let mut lapsed = Vec::new();
        for row in due {
            let memory = MemoryId::parse(&row.memory_id).map_err(|_| {
                Error::Integrity("a promotion row names a memory id that does not parse".to_owned())
            })?;
            let mut found = load(self.store, scope, memory).await?;
            let Some((current, expected)) = found.as_mut().filter(|(memory, _)| {
                memory.status != Status::Erased
                    && memory
                        .trust
                        .promotion()
                        .is_some_and(|promotion| promotion.decision().as_str() == row.decision_id)
            }) else {
                // The memory is gone, or was demoted and promoted again under
                // another Decision: this row describes nothing live.
                self.store
                    .txn(vec![delete_promotion_if(
                        &scope_id,
                        memory,
                        &row.decision_id,
                    )])
                    .await?;
                continue;
            };
            let status = current.status.label();
            current.trust = TrustClass::Assertion;
            current.updated = now;

            let decision_id = format!("{KIND_LAPSE}-{}", row.decision_id);
            self.ledger
                .append(
                    Decision::new(
                        DecisionId::new(decision_id.clone()),
                        DecisionKind::new(KIND_LAPSE),
                        Sub::new(row.promoted_by.as_str()),
                        Outcome::Allow,
                        "a promotion lapsed at its review date and the memory returned to \
                         assertion grade",
                    )
                    .with_payload(serde_json::json!({
                        "scope": scope.to_string(),
                        "memory_id": memory.to_string(),
                        "promotion_decision": row.decision_id,
                    })),
                )
                .await?;
            let mut txn = TxnBuilder::new();
            txn.push(memory_update(&scope_id, current, status, expected)?);
            txn.push(delete_promotion_if(&scope_id, memory, &row.decision_id));
            txn.push(Statement::with_params(
                "INSERT INTO review_item
                   (item_id, scope_id, kind, subject, created, state)
                 VALUES (?1, ?2, ?3, ?4, ?5, 'open')
                 ON CONFLICT DO NOTHING",
                vec![
                    Value::from(format!("rvw-{}", Uuid::now_v7().simple())),
                    Value::from(&scope_id),
                    Value::from(ReviewKind::PromotionRequest.label()),
                    Value::from(memory.to_string()),
                    Value::Integer(sql_seconds(now)?),
                ],
            ));
            match self.store.txn(txn.into_statements()).await {
                Ok(_) => lapsed.push(Lapsed {
                    memory,
                    decision: decision_id,
                }),
                Err(Error::Conflict(_)) => {}
                Err(error) => return Err(error),
            }
        }
        Ok(lapsed)
    }
}

async fn existing(
    store: &StoreHandle,
    scope: &Scope,
    memory: MemoryId,
) -> Result<(Memory, String), Error> {
    load(store, scope, memory)
        .await?
        .ok_or_else(|| Error::NotFound(format!("memory {memory} is not in this scope")))
}

fn delete_promotion(scope_id: &ScopeId, memory: MemoryId) -> Statement {
    Statement::with_params(
        "DELETE FROM promotion WHERE scope_id = ?1 AND memory_id = ?2",
        vec![Value::from(scope_id), Value::from(memory.to_string())],
    )
}

/// Delete the row only while it still names `decision`, so a sweep never
/// removes the row of a promotion made after it read.
fn delete_promotion_if(scope_id: &ScopeId, memory: MemoryId, decision: &str) -> Statement {
    Statement::with_params(
        "DELETE FROM promotion WHERE scope_id = ?1 AND memory_id = ?2 AND decision_id = ?3",
        vec![
            Value::from(scope_id),
            Value::from(memory.to_string()),
            Value::from(decision),
        ],
    )
}

/// Record that a Decision's act did not commit. Best effort: the error the
/// caller returns is the one that matters.
async fn refuse(ledger: &Ledger, kind: &str, scope: &Scope, memory: MemoryId, by: &Sub) {
    let refusal = Decision::new(
        DecisionId::new(format!(
            "{kind}_failed-{memory}-{}",
            Uuid::now_v7().simple()
        )),
        DecisionKind::new(format!("{kind}_failed")),
        by.clone(),
        Outcome::Deny,
        "the preceding decision was recorded but its act did not commit",
    )
    .with_payload(serde_json::json!({
        "scope": scope.to_string(),
        "memory_id": memory.to_string(),
    }));
    let _ = ledger.append(refusal).await;
}
