//! One chain, referenced (spec 024 B-1, B-2, B-3).
//!
//! There is exactly one hash chain in a deployment and it is the chassis's.
//! This module appends to it through [`rahi_ledger::Ledger::append`] and
//! stores references to what it appended; it links no record, holds no
//! parent hash and verifies nothing (B-1). The word *ledger* in this
//! repository always means that chain.
//!
//! Three things live here.
//!
//! - [`ReasonCode`], the closed vocabulary every Decision this product writes
//!   names (B-2), and [`validate_decision`], which refuses a Decision whose
//!   shape could carry memory content into a chain that can never forget it.
//!   [`append_decision`] validates and then appends, so an emission site has
//!   one call to make and no way to skip the check.
//! - [`DecisionRefRepo`], the `decision_ref` index table (B-3). The chain
//!   stays the authority; the table is an index into it, so a memory's audit
//!   history is a bounded indexed read rather than a scan of the chain.
//!   A reference is staged into the same transaction as the act it records,
//!   so a committed act is never missing its reference.
//! - The table's migration, [`decision_ref_migration`].
//!
//! Nothing here holds memory content. A reference names a memory, a Decision
//! and a kind; an erased memory keeps its references (B-5), which is why they
//! must never be able to hold the body.

use aicortex_types::{MemoryId, Scope};
use rahi_ledger::{Decision, Hash, Ledger};
use rahi_store::{Migration, Statement, TxnBuilder, Value};
use rahi_types::{Error, UnixSeconds};

use crate::scope_repo::{ScopeId, seconds_to_sql};

/// The decision-reference schema of spec 024.
///
/// Version 15: the next free one after 023's review tables (14). The list
/// must be contiguous (053 D-9), so a change that lands first takes the
/// number and the other renumbers this constant and nothing else.
pub const DECISION_REF_VERSION: u32 = 15;

/// The longest reviewer reason a Decision may carry anywhere in its payload,
/// in bytes: 023's limit of 500 characters, at most four UTF-8 bytes each.
/// Checked here so the chain's bound does not rest on another crate's check.
pub const MAX_REASON_BYTES: usize = 2_000;

/// The longest optional justification a Decision may carry, in bytes (B-2).
///
/// A short sentence a person typed, never a place to paste content: the value
/// reaches a chain that cannot be edited.
pub const MAX_JUSTIFICATION_BYTES: usize = 280;

/// Payload keys that would put memory content into the chain (B-2).
///
/// Matched case-insensitively at any depth. The list is a tripwire for the
/// obvious mistake, not a content scanner: the guarantee that a body never
/// arrives here is that no emission site is handed one, and
/// 014 FR-003's test, which asserts that no substring of an erased body is in
/// a serialized erasure Decision.
const CONTENT_KEYS: [&str; 8] = [
    "body",
    "content",
    "text",
    "chunk",
    "chunks",
    "embedding",
    "vector",
    "record",
];

const DECISION_REF_TABLE: &str = "CREATE TABLE decision_ref (
    scope_id TEXT NOT NULL,
    memory_id TEXT NOT NULL,
    decision_id TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN
        ('quarantine', 'review_admit', 'review_decline', 'review_acknowledge',
         'promote', 'demote', 'lapse', 'erase')),
    created INTEGER NOT NULL,
    PRIMARY KEY (scope_id, memory_id, decision_id)
)";

/// Spec 024's migration.
///
/// Additive under rahi 036 B-8 (spec 046 B-2): one new table that no older
/// binary reads or writes. An older binary that never stages a reference
/// leaves a history shorter than the chain, which is a gap in an index and
/// not a wrong answer: the chain stays the authority (D-1).
#[must_use]
pub fn decision_ref_migration() -> Migration {
    Migration::new(
        DECISION_REF_VERSION,
        "aicortex decision references",
        DECISION_REF_TABLE,
    )
    .additive()
}

/// Why a Decision was made: the closed enum of B-2.
///
/// Every Decision this product writes carries one of these as its
/// `reason_code`. The gate's seven codes mirror `aicortex_gate::Reason::code`
/// and a test asserts the two agree.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ReasonCode {
    /// The gate found a credential.
    SecretDetected,
    /// The normalized body is over the ceiling.
    TooLarge,
    /// Nothing was left once the body was normalized.
    EmptyAfterNormalization,
    /// The origin could not be established.
    OriginUnestablished,
    /// The media is not accepted.
    UnsupportedMedia,
    /// The caller is over its rate.
    RateExceeded,
    /// A policy refused the capture.
    PolicyDenied,
    /// The subject asked for the erasure.
    SubjectRequest,
    /// A user asked for the erasure.
    UserRequest,
    /// A retention rule erased it.
    RetentionPolicy,
    /// A legal hold was lifted and the memory erased.
    LegalHoldLift,
    /// A reviewer rejected a quarantined memory.
    ReviewReject,
    /// A reviewer admitted a quarantined memory.
    ReviewAdmit,
    /// A reviewer declined a promotion request.
    ReviewDecline,
    /// A reviewer acknowledged a gate override.
    ReviewAcknowledge,
    /// A reviewer resolved a contradiction.
    ReviewContradiction,
    /// A person promoted a memory to instruction grade.
    Promotion,
    /// A person returned a memory to assertion grade.
    Demotion,
    /// A promotion reached its review date.
    PromotionLapsed,
    /// A recorded Decision's act did not commit.
    ActNotCommitted,
}

impl ReasonCode {
    /// Every code, in declaration order.
    pub const ALL: [Self; 20] = [
        Self::SecretDetected,
        Self::TooLarge,
        Self::EmptyAfterNormalization,
        Self::OriginUnestablished,
        Self::UnsupportedMedia,
        Self::RateExceeded,
        Self::PolicyDenied,
        Self::SubjectRequest,
        Self::UserRequest,
        Self::RetentionPolicy,
        Self::LegalHoldLift,
        Self::ReviewReject,
        Self::ReviewAdmit,
        Self::ReviewDecline,
        Self::ReviewAcknowledge,
        Self::ReviewContradiction,
        Self::Promotion,
        Self::Demotion,
        Self::PromotionLapsed,
        Self::ActNotCommitted,
    ];

    /// The wire form a Decision carries.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::SecretDetected => "secret_detected",
            Self::TooLarge => "too_large",
            Self::EmptyAfterNormalization => "empty_after_normalization",
            Self::OriginUnestablished => "origin_unestablished",
            Self::UnsupportedMedia => "unsupported_media",
            Self::RateExceeded => "rate_exceeded",
            Self::PolicyDenied => "policy_denied",
            Self::SubjectRequest => "subject_request",
            Self::UserRequest => "user_request",
            Self::RetentionPolicy => "retention_policy",
            Self::LegalHoldLift => "legal_hold_lift",
            Self::ReviewReject => "review_reject",
            Self::ReviewAdmit => "review_admit",
            Self::ReviewDecline => "review_decline",
            Self::ReviewAcknowledge => "review_acknowledge",
            Self::ReviewContradiction => "review_contradiction",
            Self::Promotion => "promotion",
            Self::Demotion => "demotion",
            Self::PromotionLapsed => "promotion_lapsed",
            Self::ActNotCommitted => "act_not_committed",
        }
    }

    /// The code named by `text`, when it is one of the closed set.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|code| code.label() == text)
    }
}

impl core::fmt::Display for ReasonCode {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.label())
    }
}

/// What a memory's Decision did to it: the `kind` of a `decision_ref` row.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RefKind {
    /// The gate stored the memory out of sight.
    Quarantine,
    /// A reviewer admitted a quarantined memory.
    ReviewAdmit,
    /// A reviewer declined to promote it.
    ReviewDecline,
    /// A reviewer acknowledged a gate override of it.
    ReviewAcknowledge,
    /// A person promoted it to instruction grade.
    Promote,
    /// A person returned it to assertion grade.
    Demote,
    /// Its promotion lapsed at the review date.
    Lapse,
    /// It was erased.
    Erase,
}

impl RefKind {
    /// Every kind, in declaration order.
    pub const ALL: [Self; 8] = [
        Self::Quarantine,
        Self::ReviewAdmit,
        Self::ReviewDecline,
        Self::ReviewAcknowledge,
        Self::Promote,
        Self::Demote,
        Self::Lapse,
        Self::Erase,
    ];

    /// The value the `kind` column holds.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Quarantine => "quarantine",
            Self::ReviewAdmit => "review_admit",
            Self::ReviewDecline => "review_decline",
            Self::ReviewAcknowledge => "review_acknowledge",
            Self::Promote => "promote",
            Self::Demote => "demote",
            Self::Lapse => "lapse",
            Self::Erase => "erase",
        }
    }

    /// The kind named by `text`, when it is one of the closed set.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.label() == text)
    }
}

impl core::fmt::Display for RefKind {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.label())
    }
}

/// Whether `value` holds a key a Decision must not carry, at any depth.
fn names_content(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::Object(map) => map.iter().find_map(|(key, inner)| {
            if CONTENT_KEYS
                .iter()
                .any(|banned| key.eq_ignore_ascii_case(banned))
            {
                Some(key.clone())
            } else {
                names_content(inner)
            }
        }),
        serde_json::Value::Array(items) => items.iter().find_map(names_content),
        serde_json::Value::Null
        | serde_json::Value::Bool(_)
        | serde_json::Value::Number(_)
        | serde_json::Value::String(_) => None,
    }
}

/// Whether any text `reason` field, at any depth of `value`, is longer than
/// `max` bytes. A `reason` of another shape (the gate's structured verdict)
/// is not free text and is checked by its own producer.
fn reason_over(value: &serde_json::Value, max: usize) -> bool {
    match value {
        serde_json::Value::Object(map) => map.iter().any(|(key, value)| {
            (key == "reason" && value.as_str().is_some_and(|text| text.len() > max))
                || reason_over(value, max)
        }),
        serde_json::Value::Array(items) => items.iter().any(|item| reason_over(item, max)),
        _ => false,
    }
}

/// Check the record shape of B-2 and return the Decision's reason code.
///
/// A Decision names its actor and capability on the record itself; this
/// checks what is left to the payload: a `scope` string, a `reason_code` from
/// the closed enum, no key that names memory content, a justification, when
/// there is one, of at most [`MAX_JUSTIFICATION_BYTES`], and any text
/// `reason` of at most [`MAX_REASON_BYTES`].
///
/// # Errors
///
/// [`Error::Validation`] naming the field that failed. The message never
/// echoes a payload value, because the value is what must not be repeated.
pub fn validate_decision(decision: &Decision) -> Result<ReasonCode, Error> {
    let payload = decision.payload.as_value();
    let Some(object) = payload.as_object() else {
        return Err(Error::Validation(format!(
            "decision {} has no payload object, so it names no scope and no reason code",
            decision.id
        )));
    };
    let code = object
        .get("reason_code")
        .and_then(serde_json::Value::as_str)
        .and_then(ReasonCode::parse)
        .ok_or_else(|| {
            Error::Validation(format!(
                "decision {} has no reason_code from the closed set",
                decision.id
            ))
        })?;
    if object
        .get("scope")
        .and_then(serde_json::Value::as_str)
        .is_none_or(str::is_empty)
    {
        return Err(Error::Validation(format!(
            "decision {} names no scope",
            decision.id
        )));
    }
    if let Some(key) = names_content(payload) {
        return Err(Error::Validation(format!(
            "decision {} carries a {key:?} field, and memory content never enters the chain",
            decision.id
        )));
    }
    if reason_over(payload, MAX_REASON_BYTES) {
        return Err(Error::Validation(format!(
            "decision {} has a reason longer than {MAX_REASON_BYTES} bytes",
            decision.id
        )));
    }
    match object.get("justification") {
        None | Some(serde_json::Value::Null) => {}
        Some(serde_json::Value::String(text)) if text.len() <= MAX_JUSTIFICATION_BYTES => {}
        Some(_) => {
            return Err(Error::Validation(format!(
                "decision {} has a justification that is not a string of at most \
                 {MAX_JUSTIFICATION_BYTES} bytes",
                decision.id
            )));
        }
    }
    Ok(code)
}

/// Validate `decision` and append it to the chassis's chain (B-1, B-2).
///
/// The one way this product appends a new Decision: the chain is rahi's and
/// this function names only its `append`. The one exception is 014's
/// delivery of an erasure receipt: that Decision was validated when the
/// erasure was staged and its act has committed, so it is delivered exactly
/// as recorded and never refused afterwards (014 B-7, 024 D-3).
///
/// # Errors
///
/// [`Error::Validation`] from [`validate_decision`], or the ledger's error.
pub async fn append_decision(ledger: &Ledger, decision: Decision) -> Result<Hash, Error> {
    validate_decision(&decision)?;
    ledger.append(decision).await
}

/// The `decision_ref` table (B-3).
///
/// Every statement names `scope_id`, like every other statement in this
/// crate (012 B-3).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DecisionRefRepo;

impl DecisionRefRepo {
    /// The statement that records `decision` against `memory`.
    ///
    /// Idempotent: replaying the same Decision for the same memory changes
    /// nothing, because a replay is the same record and not a second event.
    #[must_use]
    pub fn statement(
        scope: &Scope,
        memory: MemoryId,
        decision_id: &str,
        kind: RefKind,
        created: UnixSeconds,
    ) -> Statement {
        Statement::with_params(
            "INSERT INTO decision_ref (scope_id, memory_id, decision_id, kind, created)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT (scope_id, memory_id, decision_id) DO NOTHING",
            vec![
                Value::from(&ScopeId::of(scope)),
                Value::from(memory.to_string()),
                Value::from(decision_id),
                Value::from(kind.label()),
                Value::Integer(seconds_to_sql(created)),
            ],
        )
    }

    /// Stage the reference into the caller's transaction, the one that
    /// carries the act the Decision records.
    pub fn stage(
        txn: &mut TxnBuilder,
        scope: &Scope,
        memory: MemoryId,
        decision_id: &str,
        kind: RefKind,
        created: UnixSeconds,
    ) {
        txn.push(Self::statement(scope, memory, decision_id, kind, created));
    }
}
