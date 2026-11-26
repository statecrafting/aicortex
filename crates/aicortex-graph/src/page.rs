//! The entity page: a computed, cited view of one entity (spec 056).
//!
//! [`entity_page`] answers "what does the store hold about this entity, and
//! why": the header of 017 B-1, the active edges grouped by predicate, the
//! open contradictions side by side, and the most recent mentions. It is a
//! pure read. It takes a [`ReadStore`], so it cannot write; it opens no
//! socket; it persists and caches nothing, and it generates no text: every
//! value on a page is a stored value or a fixed label of this crate (B-1).
//!
//! A fact is shown only while at least one memory it was derived from is still
//! live in the scope (B-4). That is decided here, from the memory rows, and
//! not from the edge's own status, so a page is correct in the window between
//! an erasure and the cleanup 017 propagates to the edges.
//!
//! This crate returns memories and literals as data and serializes neither.
//! The surfaces envelope every memory body and every literal on a page through
//! `aicortex-recall` (019, B-7); nothing here has a serializer for them.

use core::fmt;
use core::str::FromStr;

use aicortex_store::ReadStore;
use aicortex_types::{Memory, MemoryId};
use rahi_store::Value;
use rahi_types::{Error, UnixSeconds};
use serde::Deserialize;

use crate::edge::EdgeId;
use crate::entity::{Entity, EntityId};
use crate::vocabulary::Predicate;

/// Facts per predicate when a request names no bound.
pub const DEFAULT_FACTS: u32 = 10;
/// The most facts one predicate group returns.
pub const MAX_FACTS: u32 = 50;
/// Contradictions when a request names no bound.
pub const DEFAULT_CONTRADICTIONS: u32 = 10;
/// The most contradictions one page returns.
pub const MAX_CONTRADICTIONS: u32 = 50;
/// Mentioning memories when a request names no bound.
pub const DEFAULT_MENTIONS: u32 = 10;
/// The most mentioning memories one page returns.
pub const MAX_MENTIONS: u32 = 50;
/// The most source memories one fact lists; the rest are counted.
pub const MAX_FACT_SOURCES: u32 = 8;
/// The most byte ranges one mentioning memory lists.
pub const MAX_RANGES: u32 = 50;
/// The most prior entity ids a header lists.
pub const MAX_MERGED: u32 = 100;

/// Spell a cluster of entity ids: the entity and every entity merged into it
/// (017 B-2). Statements using it bind the scope as `?1` and the entity as `?3`,
/// and the statement's own value as `?2`.
macro_rules! cluster {
    () => {
        "(SELECT entity_id FROM entity WHERE scope_id = ?1 \
          AND (entity_id = ?3 OR merged_into = ?3))"
    };
}

/// A memory's trust class as a rank, strongest first (B-5). Instruction grade
/// is the one a human promoted; evidence is recorded fact; an assertion is a
/// claim. `m` is the memory row.
macro_rules! rank {
    () => {
        "CASE m.trust WHEN 'instruction' THEN 0 WHEN 'evidence' THEN 1 ELSE 2 END"
    };
}

/// Whether memory row `m` still stands behind a fact at the moment bound by
/// `$as_of` (a statement parameter): active, which is neither erased nor
/// superseded and also not waiting in quarantine or expired, and not newer
/// than the page's time (B-4).
macro_rules! live {
    ($as_of:literal) => {
        concat!("m.status = 'active' AND m.created <= ", $as_of)
    };
}

const FACTS_SQL: &str = concat!(
    "SELECT edge_id, outgoing, subject, object_entity, object_literal, confidence, extractor, \
            extractor_version, valid_from, valid_until, rank, sources
     FROM (
       SELECT e.edge_id AS edge_id,
              e.subject IN ",
    cluster!(),
    " AS outgoing,
              e.subject AS subject, e.object_entity AS object_entity,
              e.object_literal AS object_literal, e.confidence AS confidence,
              e.extractor AS extractor, e.extractor_version AS extractor_version,
              e.valid_from AS valid_from, e.valid_until AS valid_until,
              (SELECT MIN(",
    rank!(),
    ") FROM edge_source s JOIN memory m
                 ON m.scope_id = s.scope_id AND m.id = s.memory_id
               WHERE s.scope_id = e.scope_id AND s.edge_id = e.edge_id AND ",
    live!("?4"),
    ") AS rank,
              (SELECT COUNT(*) FROM edge_source s JOIN memory m
                 ON m.scope_id = s.scope_id AND m.id = s.memory_id
               WHERE s.scope_id = e.scope_id AND s.edge_id = e.edge_id AND ",
    live!("?4"),
    ") AS sources
       FROM edge e
       WHERE e.scope_id = ?1 AND e.status = 'active'
         AND (?2 = '' OR e.predicate = ?2)
         AND (?5 = '' OR e.edge_id = ?5)
         AND (?5 <> '' OR e.subject IN ",
    cluster!(),
    " OR e.object_entity IN ",
    cluster!(),
    ")
         AND e.valid_from <= ?4 AND COALESCE(e.valid_until, 9223372036854775807) > ?4
     )
     WHERE rank IS NOT NULL
       AND (?6 = 0 OR rank > ?7
            OR (rank = ?7 AND (confidence < ?8 OR (confidence = ?8 AND edge_id > ?9))))
     ORDER BY rank, confidence DESC, edge_id LIMIT ?10"
);

const SOURCES_SQL: &str = concat!(
    "SELECT s.memory_id AS memory_id
     FROM edge_source s JOIN memory m ON m.scope_id = s.scope_id AND m.id = s.memory_id
     WHERE s.scope_id = ?1 AND s.edge_id = ?2 AND ",
    live!("?3"),
    "
     ORDER BY ",
    rank!(),
    ", s.memory_id LIMIT ?4"
);

const CONTRADICTIONS_SQL: &str = concat!(
    "SELECT contradiction_id, edge_a, edge_b, predicate, detected, status
     FROM contradiction
     WHERE scope_id = ?1 AND status = 'open' AND detected <= ?2
       AND subject IN ",
    cluster!(),
    "
       AND (?4 = 0 OR detected > ?5 OR (detected = ?5 AND contradiction_id > ?6))
     ORDER BY detected, contradiction_id LIMIT ?7"
);

const MENTIONING_SQL: &str = concat!(
    "SELECT m.id AS id, m.created AS created, m.origin_erased AS origin_erased,
            m.record AS record
     FROM memory m
     WHERE m.scope_id = ?1 AND ",
    live!("?2"),
    "
       AND EXISTS (SELECT 1 FROM mention x
                   WHERE x.scope_id = m.scope_id AND x.memory_id = m.id
                     AND x.entity_id IN ",
    cluster!(),
    ")
       AND (?4 = 0 OR m.created < ?5 OR (m.created = ?5 AND m.id > ?6))
     ORDER BY m.created DESC, m.id LIMIT ?7"
);

const RANGES_SQL: &str = concat!(
    "SELECT byte_start, byte_end, confidence FROM mention
     WHERE scope_id = ?1 AND memory_id = ?2 AND entity_id IN ",
    cluster!(),
    "
     ORDER BY byte_start, byte_end LIMIT ?4"
);

const MERGED_SQL: &str = "SELECT absorbed FROM entity_merge
     WHERE scope_id = ?1 AND survivor = ?2 AND reversed_at IS NULL AND merged_at <= ?3
     ORDER BY absorbed LIMIT ?4";

const END_SQL: &str = "SELECT COALESCE(s.entity_id, e.entity_id) AS entity_id,
            COALESCE(s.canonical, e.canonical) AS canonical
     FROM entity e LEFT JOIN entity s
       ON s.scope_id = e.scope_id AND s.entity_id = e.merged_into
     WHERE e.scope_id = ?1 AND e.entity_id = ?2";

/// Which end of an edge the page's entity is (B-3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    /// The entity is the edge's subject.
    Outgoing,
    /// The entity is the edge's object.
    Incoming,
}

impl Direction {
    /// The stable label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Outgoing => "outgoing",
            Self::Incoming => "incoming",
        }
    }
}

/// The far end of a fact (B-3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FactEnd {
    /// Another entity, named as its survivor if it was merged. The name is a
    /// derived value (B-7).
    Entity {
        /// Its id.
        id: EntityId,
        /// The name it is shown under.
        name: String,
    },
    /// Literal text extracted from a memory. A surface MUST envelope it (B-7).
    Literal(String),
}

/// One cited fact: an active edge with the memories that still support it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fact {
    /// The edge.
    pub edge: EdgeId,
    /// Which end of the edge the page's entity is.
    pub direction: Direction,
    /// The other end.
    pub other: FactEnd,
    /// The extractor's confidence in permille.
    pub confidence: u16,
    /// The extractor's name.
    pub extractor: String,
    /// The extractor version.
    pub extractor_version: u32,
    /// When the fact began to hold.
    pub valid_from: UnixSeconds,
    /// When it stopped holding, if it did.
    pub valid_until: Option<UnixSeconds>,
    /// The strongest trust class label among the live sources (B-5).
    pub trust: &'static str,
    /// Live source memories, strongest first, at most [`MAX_FACT_SOURCES`].
    /// Never empty (B-4).
    pub sources: Vec<MemoryId>,
    /// How many live source memories the fact has in all.
    pub source_count: u64,
}

/// The facts of one predicate, in the order of B-5.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FactGroup {
    /// The predicate.
    pub predicate: Predicate,
    /// The facts on this slice.
    pub facts: Vec<Fact>,
    /// Where the next slice of this group resumes, when it was truncated.
    pub next: Option<Continuation>,
}

/// One open contradiction (017 B-10): every side, and no winner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PageContradiction {
    /// Its id. Until spec 023 gives the review queue a record of its own, this
    /// is the review item the contradiction waits on (D-5).
    pub id: String,
    /// The shared predicate.
    pub predicate: Predicate,
    /// When it was detected.
    pub detected: UnixSeconds,
    /// Its status, which is `open` on a page.
    pub status: String,
    /// The disagreeing facts, lesser edge id first.
    pub sides: Vec<Fact>,
}

/// A byte range of a memory that mentions the entity (017 B-3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MentionRange {
    /// The first byte.
    pub start: u64,
    /// One past the last byte.
    pub end: u64,
    /// The extractor's confidence in permille.
    pub confidence: u16,
}

/// A live memory that mentions the entity. A surface MUST envelope it (B-7).
#[derive(Clone, Debug)]
pub struct MentioningMemory {
    /// The memory.
    pub memory: Memory,
    /// Its `origin_erased` marker, which the envelope carries (019 B-1).
    pub origin_erased: bool,
    /// Where it mentions the entity, in byte order.
    pub ranges: Vec<MentionRange>,
}

/// The header of a page (B-3 section 1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Header {
    /// The entity as 017 B-1 presents it, with its survivor's identity if the
    /// requested id was merged.
    pub entity: Entity,
    /// The ids of entities merged into this one (017 B-2), still reversible.
    pub merged: Vec<EntityId>,
}

/// The sections a continuation can resume (B-6).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Continuation {
    /// Facts of one predicate after the fact with this sort key.
    Facts {
        /// The group.
        predicate: Predicate,
        /// The strongest-trust rank of the last fact returned.
        rank: u8,
        /// Its confidence in permille.
        confidence: u16,
        /// Its edge id.
        edge: EdgeId,
    },
    /// Contradictions after this one.
    Contradictions {
        /// When the last one returned was detected.
        detected: u64,
        /// Its id.
        id: String,
    },
    /// Mentioning memories after this one.
    Mentions {
        /// The last one returned was created at this time.
        created: u64,
        /// Its id.
        memory: MemoryId,
    },
}

impl fmt::Display for Continuation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Facts {
                predicate,
                rank,
                confidence,
                edge,
            } => write!(f, "facts:{}:{rank}:{confidence}:{edge}", predicate.as_str()),
            Self::Contradictions { detected, id } => write!(f, "contradictions:{detected}:{id}"),
            Self::Mentions { created, memory } => write!(f, "mentions:{created}:{memory}"),
        }
    }
}

impl FromStr for Continuation {
    type Err = Error;

    fn from_str(text: &str) -> Result<Self, Error> {
        let refused = || {
            Error::Validation(format!(
                "continuation {text:?} is not a token this page issued; \
                 pass back a `next` value unchanged"
            ))
        };
        let mut parts = text.splitn(5, ':');
        match (
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
        ) {
            (Some("facts"), Some(predicate), Some(rank), Some(confidence), Some(edge)) => {
                Ok(Self::Facts {
                    predicate: Predicate::parse(predicate).ok_or_else(refused)?,
                    rank: rank.parse().map_err(|_| refused())?,
                    confidence: confidence.parse().map_err(|_| refused())?,
                    edge: EdgeId::new(edge),
                })
            }
            (Some("contradictions"), Some(detected), Some(id), None, None) => {
                Ok(Self::Contradictions {
                    detected: detected.parse().map_err(|_| refused())?,
                    id: id.to_owned(),
                })
            }
            (Some("mentions"), Some(created), Some(memory), None, None) => Ok(Self::Mentions {
                created: created.parse().map_err(|_| refused())?,
                memory: MemoryId::parse(memory).map_err(|_| refused())?,
            }),
            _ => Err(refused()),
        }
    }
}

/// The bounds of one page read (B-6). Out-of-range values are clamped, never
/// refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PageBounds {
    /// Facts per predicate, `1..=MAX_FACTS`.
    pub facts_per_predicate: u32,
    /// Contradictions, `1..=MAX_CONTRADICTIONS`.
    pub contradictions: u32,
    /// Mentioning memories, `1..=MAX_MENTIONS`.
    pub mentions: u32,
    /// Resume one section from the `next` a previous read returned. Only that
    /// section is read; the others come back empty, so a client walking one
    /// section does not pay for the rest.
    pub resume: Option<Continuation>,
}

impl Default for PageBounds {
    fn default() -> Self {
        Self {
            facts_per_predicate: DEFAULT_FACTS,
            contradictions: DEFAULT_CONTRADICTIONS,
            mentions: DEFAULT_MENTIONS,
            resume: None,
        }
    }
}

/// A slice of one section and where the next one resumes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Slice<T> {
    /// The items on this slice.
    pub items: Vec<T>,
    /// Where the next slice resumes, when this one was truncated.
    pub next: Option<Continuation>,
}

impl<T> Slice<T> {
    const fn empty() -> Self {
        Self {
            items: Vec::new(),
            next: None,
        }
    }
}

/// One entity page (B-3).
#[derive(Clone, Debug)]
pub struct EntityPage {
    /// The moment the page was read as of.
    pub as_of: UnixSeconds,
    /// Section 1.
    pub header: Header,
    /// Section 2: groups in the vocabulary's order, empty groups omitted.
    pub facts: Vec<FactGroup>,
    /// Section 3.
    pub contradictions: Slice<PageContradiction>,
    /// Section 4.
    pub mentions: Slice<MentioningMemory>,
}

#[derive(Debug, Deserialize)]
struct FactRow {
    edge_id: String,
    outgoing: i64,
    subject: String,
    object_entity: Option<String>,
    object_literal: Option<String>,
    confidence: i64,
    extractor: String,
    extractor_version: i64,
    valid_from: i64,
    valid_until: Option<i64>,
    rank: i64,
    sources: i64,
}

#[derive(Debug, Deserialize)]
struct SourceRow {
    memory_id: String,
}

#[derive(Debug, Deserialize)]
struct ContradictionRow {
    contradiction_id: String,
    edge_a: String,
    edge_b: String,
    predicate: String,
    detected: i64,
    status: String,
}

#[derive(Debug, Deserialize)]
struct MemoryRow {
    id: String,
    created: i64,
    origin_erased: i64,
    record: String,
}

#[derive(Debug, Deserialize)]
struct RangeRow {
    byte_start: i64,
    byte_end: i64,
    confidence: i64,
}

#[derive(Debug, Deserialize)]
struct MergedRow {
    absorbed: String,
}

#[derive(Debug, Deserialize)]
struct EndRow {
    entity_id: String,
    canonical: String,
}

fn seconds(at: UnixSeconds) -> i64 {
    i64::try_from(at.get()).unwrap_or(i64::MAX)
}

fn unix(value: i64) -> UnixSeconds {
    UnixSeconds::new(u64::try_from(value).unwrap_or(0))
}

fn count(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

/// The label of a rank the `rank!` expression computes, strongest first.
fn trust_of(rank: i64) -> Result<&'static str, Error> {
    match rank {
        0 => Ok("instruction"),
        1 => Ok("evidence"),
        2 => Ok("assertion"),
        other => Err(Error::Integrity(format!(
            "trust rank {other} is not one the page computes"
        ))),
    }
}

/// The bounds of a read of one entity at one moment.
struct Reader<'a, S: ReadStore> {
    store: &'a S,
    scope_id: &'a str,
    entity: &'a EntityId,
    as_of: i64,
}

/// Where a facts read resumes, as the statement's parameters spell it.
struct After {
    rank: i64,
    confidence: i64,
    edge: String,
}

impl<S: ReadStore> Reader<'_, S> {
    /// The statement parameters `?1` to `?3`, `second` being the statement's own.
    fn base(&self, second: Value) -> Vec<Value> {
        vec![
            Value::from(self.scope_id),
            second,
            Value::from(self.entity.as_str()),
        ]
    }

    async fn sources(&self, edge: &str) -> Result<Vec<MemoryId>, Error> {
        let rows: Vec<SourceRow> = self
            .store
            .query(
                SOURCES_SQL,
                vec![
                    Value::from(self.scope_id),
                    Value::from(edge),
                    Value::Integer(self.as_of),
                    Value::Integer(i64::from(MAX_FACT_SOURCES)),
                ],
            )
            .await?;
        rows.into_iter()
            .map(|row| {
                MemoryId::parse(&row.memory_id)
                    .map_err(|e| Error::Integrity(format!("edge source id: {e}")))
            })
            .collect()
    }

    async fn end_of(&self, id: &str) -> Result<FactEnd, Error> {
        let rows: Vec<EndRow> = self
            .store
            .query(END_SQL, vec![Value::from(self.scope_id), Value::from(id)])
            .await?;
        let row = rows.into_iter().next().ok_or_else(|| {
            Error::Integrity(format!("edge names entity {id} that is not in its scope"))
        })?;
        Ok(FactEnd::Entity {
            id: EntityId::new(row.entity_id),
            name: row.canonical,
        })
    }

    async fn fact(&self, row: FactRow) -> Result<Fact, Error> {
        let direction = if row.outgoing == 0 {
            Direction::Incoming
        } else {
            Direction::Outgoing
        };
        let other = match (direction, row.object_entity, row.object_literal) {
            (Direction::Outgoing, Some(entity), None) => self.end_of(&entity).await?,
            (Direction::Outgoing, None, Some(text)) => FactEnd::Literal(text),
            (Direction::Incoming, _, None) => self.end_of(&row.subject).await?,
            _ => {
                return Err(Error::Integrity(format!(
                    "edge {} has no single object",
                    row.edge_id
                )));
            }
        };
        let sources = self.sources(&row.edge_id).await?;
        Ok(Fact {
            edge: EdgeId::new(row.edge_id),
            direction,
            other,
            confidence: u16::try_from(row.confidence).unwrap_or(0),
            extractor: row.extractor,
            extractor_version: u32::try_from(row.extractor_version).unwrap_or(0),
            valid_from: unix(row.valid_from),
            valid_until: row.valid_until.map(unix),
            trust: trust_of(row.rank)?,
            sources,
            source_count: count(row.sources),
        })
    }

    /// Rows of the facts statement: of one `predicate` (or any, when empty), or
    /// of one `edge` (or any, when empty), after `after`.
    async fn fact_rows(
        &self,
        predicate: &str,
        edge: &str,
        after: Option<&After>,
        limit: i64,
    ) -> Result<Vec<FactRow>, Error> {
        let mut params = self.base(Value::from(predicate));
        params.push(Value::Integer(self.as_of));
        params.push(Value::from(edge));
        match after {
            Some(after) => params.extend([
                Value::Integer(1),
                Value::Integer(after.rank),
                Value::Integer(after.confidence),
                Value::from(after.edge.as_str()),
            ]),
            None => params.extend([
                Value::Integer(0),
                Value::Integer(0),
                Value::Integer(0),
                Value::from(""),
            ]),
        }
        params.push(Value::Integer(limit));
        self.store.query(FACTS_SQL, params).await
    }

    async fn group(
        &self,
        predicate: Predicate,
        after: Option<&Continuation>,
        limit: u32,
    ) -> Result<Option<FactGroup>, Error> {
        let after = match after {
            Some(Continuation::Facts {
                rank,
                confidence,
                edge,
                ..
            }) => Some(After {
                rank: i64::from(*rank),
                confidence: i64::from(*confidence),
                edge: edge.as_str().to_owned(),
            }),
            _ => None,
        };
        let mut rows = self
            .fact_rows(predicate.as_str(), "", after.as_ref(), i64::from(limit) + 1)
            .await?;
        let truncated = truncate(&mut rows, limit);
        let next = match rows.last() {
            Some(last) if truncated => Some(Continuation::Facts {
                predicate,
                rank: u8::try_from(last.rank).unwrap_or(u8::MAX),
                confidence: u16::try_from(last.confidence).unwrap_or(0),
                edge: EdgeId::new(last.edge_id.clone()),
            }),
            _ => None,
        };
        if rows.is_empty() {
            return Ok(None);
        }
        let mut facts = Vec::with_capacity(rows.len());
        for row in rows {
            facts.push(self.fact(row).await?);
        }
        Ok(Some(FactGroup {
            predicate,
            facts,
            next,
        }))
    }

    async fn contradictions(
        &self,
        after: Option<&Continuation>,
        limit: u32,
    ) -> Result<Slice<PageContradiction>, Error> {
        let mut params = self.base(Value::Integer(self.as_of));
        match after {
            Some(Continuation::Contradictions { detected, id }) => params.extend([
                Value::Integer(1),
                Value::Integer(i64::try_from(*detected).unwrap_or(i64::MAX)),
                Value::from(id.as_str()),
            ]),
            _ => params.extend([Value::Integer(0), Value::Integer(0), Value::from("")]),
        }
        params.push(Value::Integer(i64::from(limit) + 1));
        let mut rows: Vec<ContradictionRow> = self.store.query(CONTRADICTIONS_SQL, params).await?;
        let truncated = truncate(&mut rows, limit);
        let next = match rows.last() {
            Some(last) if truncated => Some(Continuation::Contradictions {
                detected: count(last.detected),
                id: last.contradiction_id.clone(),
            }),
            _ => None,
        };
        let mut items = Vec::with_capacity(rows.len());
        for row in rows {
            let predicate = Predicate::parse(&row.predicate).ok_or_else(|| {
                Error::Integrity(format!(
                    "stored predicate {:?} is not in the vocabulary",
                    row.predicate
                ))
            })?;
            let mut sides = Vec::with_capacity(2);
            for edge in [&row.edge_a, &row.edge_b] {
                let found = self.fact_rows("", edge, None, 1).await?;
                if let Some(side) = found.into_iter().next() {
                    sides.push(self.fact(side).await?);
                }
            }
            // A side with no live source is not a side the page may cite (B-4),
            // and one side alone is no contradiction, so the record is left out
            // until its review settles it.
            if sides.len() == 2 {
                items.push(PageContradiction {
                    id: row.contradiction_id,
                    predicate,
                    detected: unix(row.detected),
                    status: row.status,
                    sides,
                });
            }
        }
        Ok(Slice { items, next })
    }

    async fn mentions(
        &self,
        after: Option<&Continuation>,
        limit: u32,
    ) -> Result<Slice<MentioningMemory>, Error> {
        let mut params = self.base(Value::Integer(self.as_of));
        match after {
            Some(Continuation::Mentions { created, memory }) => params.extend([
                Value::Integer(1),
                Value::Integer(i64::try_from(*created).unwrap_or(i64::MAX)),
                Value::from(memory.to_string()),
            ]),
            _ => params.extend([Value::Integer(0), Value::Integer(0), Value::from("")]),
        }
        params.push(Value::Integer(i64::from(limit) + 1));
        let mut rows: Vec<MemoryRow> = self.store.query(MENTIONING_SQL, params).await?;
        let truncated = truncate(&mut rows, limit);
        let mut items = Vec::with_capacity(rows.len());
        let mut next = None;
        for row in rows {
            let id = MemoryId::parse(&row.id)
                .map_err(|e| Error::Integrity(format!("mentioning memory id: {e}")))?;
            let memory: Memory = serde_json::from_str(&row.record).map_err(|e| {
                Error::Integrity(format!("memory {id} does not read back as a memory: {e}"))
            })?;
            let ranges: Vec<RangeRow> = self
                .store
                .query(
                    RANGES_SQL,
                    vec![
                        Value::from(self.scope_id),
                        Value::from(row.id.as_str()),
                        Value::from(self.entity.as_str()),
                        Value::Integer(i64::from(MAX_RANGES)),
                    ],
                )
                .await?;
            next = Some(Continuation::Mentions {
                created: count(row.created),
                memory: id,
            });
            items.push(MentioningMemory {
                memory,
                origin_erased: row.origin_erased != 0,
                ranges: ranges
                    .into_iter()
                    .map(|range| MentionRange {
                        start: count(range.byte_start),
                        end: count(range.byte_end),
                        confidence: u16::try_from(range.confidence).unwrap_or(0),
                    })
                    .collect(),
            });
        }
        Ok(Slice {
            items,
            next: next.filter(|_| truncated),
        })
    }

    async fn merged(&self) -> Result<Vec<EntityId>, Error> {
        let rows: Vec<MergedRow> = self
            .store
            .query(
                MERGED_SQL,
                vec![
                    Value::from(self.scope_id),
                    Value::from(self.entity.as_str()),
                    Value::Integer(self.as_of),
                    Value::Integer(i64::from(MAX_MERGED)),
                ],
            )
            .await?;
        Ok(rows
            .into_iter()
            .map(|row| EntityId::new(row.absorbed))
            .collect())
    }
}

/// Drop the probe row a read fetched past `limit`; whether there was one.
fn truncate<T>(rows: &mut Vec<T>, limit: u32) -> bool {
    let limit = usize::try_from(limit).unwrap_or(usize::MAX);
    let more = rows.len() > limit;
    rows.truncate(limit);
    more
}

/// The page for `entity` in `scope_id`, as of `as_of` (B-1).
///
/// Memories newer than `as_of` are not on the page, and an edge is a fact only
/// while its validity interval holds `as_of`. Which memories are live is the
/// store's current state: the store keeps no history of an erasure or a
/// supersession, so a page as of a past moment never shows what has since been
/// forgotten (D-3). The header is the entity's current state.
///
/// `None` when the entity is not in the scope, which a surface answers exactly
/// as it answers any unknown id.
///
/// # Errors
///
/// Store errors, and [`Error::Integrity`] for stored values outside their
/// vocabularies or a record that does not read back as a memory.
pub async fn entity_page(
    store: &impl ReadStore,
    scope_id: &str,
    entity: &EntityId,
    as_of: UnixSeconds,
    bounds: &PageBounds,
) -> Result<Option<EntityPage>, Error> {
    let Some(found) = crate::entity::get(store, scope_id, entity).await? else {
        return Ok(None);
    };
    let reader = Reader {
        store,
        scope_id,
        entity: &found.id,
        as_of: seconds(as_of),
    };
    let facts_limit = bounds.facts_per_predicate.clamp(1, MAX_FACTS);
    let contradictions_limit = bounds.contradictions.clamp(1, MAX_CONTRADICTIONS);
    let mentions_limit = bounds.mentions.clamp(1, MAX_MENTIONS);
    let mut facts = Vec::new();
    let mut contradictions = Slice::empty();
    let mut mentions = Slice::empty();
    match &bounds.resume {
        None => {
            for predicate in Predicate::ALL {
                facts.extend(reader.group(predicate, None, facts_limit).await?);
            }
            contradictions = reader.contradictions(None, contradictions_limit).await?;
            mentions = reader.mentions(None, mentions_limit).await?;
        }
        Some(token @ Continuation::Facts { predicate, .. }) => {
            facts.extend(reader.group(*predicate, Some(token), facts_limit).await?);
        }
        Some(token @ Continuation::Contradictions { .. }) => {
            contradictions = reader
                .contradictions(Some(token), contradictions_limit)
                .await?;
        }
        Some(token @ Continuation::Mentions { .. }) => {
            mentions = reader.mentions(Some(token), mentions_limit).await?;
        }
    }
    let merged = reader.merged().await?;
    Ok(Some(EntityPage {
        as_of,
        header: Header {
            entity: found,
            merged,
        },
        facts,
        contradictions,
        mentions,
    }))
}
