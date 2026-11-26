//! A memory's audit history: its lifecycle rows and the Decisions that
//! reference it, as one timeline (spec 024 B-4, B-5).
//!
//! The history is a join, not a scan of the chain. Its sources are the memory
//! row (creation, and how it ended), the source log (each merge), the
//! derivation rows (what a correction corrects), and the `decision_ref` index
//! of [`crate::decision_ref`]. One bounded statement reads all of them under
//! the caller's scope, so the cost is the memory's own events and does not
//! grow with the length of the chain (FR-004).
//!
//! An erased memory keeps its tombstone row and its references, so its
//! history stays readable and says that it was erased and by which Decision,
//! while the content is gone (B-5). Erasure also removes the source log and
//! the derivation rows of the memory (014 B-7), so the merges and the
//! correction target an erased memory had are not part of what remains. The
//! chain is never read and never rewritten here.
//!
//! No event carries content: a kind, a time, a Decision id where one exists,
//! and the id of a related memory.

use aicortex_types::{MemoryId, Scope};
use rahi_store::{StoreHandle, Value};
use rahi_types::{Error, UnixSeconds};
use serde::{Deserialize, Serialize, Serializer};

use crate::decision_ref::RefKind;
use crate::scope_repo::ScopeId;

/// The most events one history returns.
///
/// A memory with more than this many events is not a case that has happened;
/// the ceiling keeps the read bounded if it ever does, and
/// [`History::truncated`] says so rather than silently dropping the tail.
pub const MAX_HISTORY_EVENTS: u32 = 500;

/// One row per event, each leg of the union naming the scope (012 B-3).
///
/// The legs, in order: creation; each merge after the first source; how a
/// superseded memory ended, as a correction when its successor is one; what a
/// memory derived from, as a correction's target when it is one; an expiry or
/// an erasure; and the Decisions that reference it. `ord` breaks a tie
/// between kinds of event in the same second so the story is told the right
/// way round, and `seq` orders events of one kind within it: a source's
/// ordinal, a derivation's position, and for a reference the row's insertion
/// order, which is the order the acts committed in.
const HISTORY_SQL: &str = "SELECT event, at, decision, related FROM (
    SELECT 'created' AS event, m.created AS at, NULL AS decision, NULL AS related, 0 AS ord, 0 AS seq
        FROM memory m WHERE m.scope_id = ?1 AND m.id = ?2
    UNION ALL
    SELECT 'merged', s.ingested_at, NULL, NULL, 1, s.ordinal
        FROM memory_source s
        WHERE s.scope_id = ?1 AND s.memory_id = ?2 AND s.ordinal > 0
    UNION ALL
    SELECT CASE WHEN c.kind = 'correction' THEN 'corrected' ELSE 'superseded' END,
           m.updated, NULL, m.superseded_by, 2, 0
        FROM memory m
        LEFT JOIN memory c ON c.scope_id = m.scope_id AND c.id = m.superseded_by
        WHERE m.scope_id = ?1 AND m.id = ?2 AND m.status = 'superseded'
    UNION ALL
    SELECT CASE WHEN m.kind = 'correction' THEN 'corrects' ELSE 'derived' END,
           m.created, NULL, d.parent_id, 1, d.position
        FROM memory_derivation d
        JOIN memory m ON m.scope_id = d.scope_id AND m.id = d.memory_id
        WHERE d.scope_id = ?1 AND d.memory_id = ?2
    UNION ALL
    SELECT m.status, m.updated, NULL, NULL, 4, 0
        FROM memory m
        WHERE m.scope_id = ?1 AND m.id = ?2 AND m.status IN ('expired', 'erased')
    UNION ALL
    SELECT r.kind, r.created, r.decision_id, NULL, 3, r.rowid
        FROM decision_ref r WHERE r.scope_id = ?1 AND r.memory_id = ?2
) ORDER BY at, ord, seq, event, decision, related LIMIT ?3";

/// What happened to a memory.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EventKind {
    /// The memory was stored.
    Created,
    /// The same content was captured again and merged into it.
    Merged,
    /// A later memory replaced it; the successor is the related id.
    Superseded,
    /// A correction replaced it; the correction is the related id.
    Corrected,
    /// This memory is a correction; the memory it corrects is the related id.
    Corrects,
    /// This memory was derived from another, the related id.
    Derived,
    /// It passed the moment it was true until.
    Expired,
    /// Its content was erased; the tombstone and this history remain.
    Erased,
    /// A Decision about it, of the named kind, with its id.
    Decision(RefKind),
}

impl EventKind {
    /// The label the API and the store's own rows use.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Merged => "merged",
            Self::Superseded => "superseded",
            Self::Corrected => "corrected",
            Self::Corrects => "corrects",
            Self::Derived => "derived",
            Self::Expired => "expired",
            Self::Erased => "erased",
            Self::Decision(kind) => kind.label(),
        }
    }

    /// The event a row names, when it names one this module knows.
    fn parse(text: &str) -> Option<Self> {
        match text {
            "created" => Some(Self::Created),
            "merged" => Some(Self::Merged),
            "superseded" => Some(Self::Superseded),
            "corrected" => Some(Self::Corrected),
            "corrects" => Some(Self::Corrects),
            "derived" => Some(Self::Derived),
            "expired" => Some(Self::Expired),
            "erased" => Some(Self::Erased),
            other => RefKind::parse(other).map(Self::Decision),
        }
    }
}

impl Serialize for EventKind {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.label())
    }
}

/// One entry of the timeline.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct HistoryEvent {
    /// What happened.
    pub event: EventKind,
    /// When, in seconds since the epoch.
    pub at: u64,
    /// The Decision that records it, where one exists.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decision: Option<String>,
    /// The memory on the other end of a supersession, correction or
    /// derivation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub related: Option<MemoryId>,
}

/// A memory's timeline, oldest first.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct History {
    /// The memory.
    pub memory: MemoryId,
    /// Its events.
    pub events: Vec<HistoryEvent>,
    /// Whether more events exist than [`MAX_HISTORY_EVENTS`] returned.
    pub truncated: bool,
    /// How many store statements this read issued (FR-004). It is one, and
    /// the number does not depend on how long the chain is.
    pub statements: u32,
}

#[derive(Debug, Deserialize)]
struct EventRow {
    event: String,
    at: i64,
    decision: Option<String>,
    related: Option<String>,
}

/// The merged timeline of `memory` in `scope` (B-4).
///
/// `query_consistent`, the leader: an audit read that missed an event
/// committed a moment ago would say the history was shorter than it was, and
/// the answer is not on a hot path.
///
/// # Errors
///
/// [`Error::NotFound`] when the memory is not in the scope, which is also
/// what another scope's memory looks like; the store's error;
/// [`Error::Integrity`] for a row that names an event this build does not
/// know or a negative time.
pub async fn history(
    store: &StoreHandle,
    scope: &Scope,
    memory: MemoryId,
) -> Result<History, Error> {
    let scope_id = ScopeId::of(scope);
    let rows: Vec<EventRow> = store
        .query_consistent(
            HISTORY_SQL,
            vec![
                Value::from(&scope_id),
                Value::from(memory.to_string()),
                Value::Integer(i64::from(MAX_HISTORY_EVENTS) + 1),
            ],
        )
        .await?;
    let statements = 1;
    if rows.is_empty() {
        return Err(Error::NotFound(format!(
            "memory {memory} is not in this scope"
        )));
    }
    let truncated = rows.len() > MAX_HISTORY_EVENTS as usize;
    let events = rows
        .into_iter()
        .take(MAX_HISTORY_EVENTS as usize)
        .map(|row| {
            let event = EventKind::parse(&row.event).ok_or_else(|| {
                Error::Integrity(format!(
                    "memory {memory} has an unknown event {:?}",
                    row.event
                ))
            })?;
            let at = u64::try_from(row.at)
                .map(UnixSeconds::new)
                .map_err(|_| Error::Integrity(format!("memory {memory} has a negative time")))?;
            let related = row
                .related
                .map(|id| {
                    MemoryId::parse(&id).map_err(|_| {
                        Error::Integrity(format!("memory {memory} relates to a malformed id"))
                    })
                })
                .transpose()?;
            Ok(HistoryEvent {
                event,
                at: at.get(),
                decision: row.decision,
                related,
            })
        })
        .collect::<Result<Vec<_>, Error>>()?;
    Ok(History {
        memory,
        events,
        truncated,
        statements,
    })
}
