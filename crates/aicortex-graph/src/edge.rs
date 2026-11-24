//! Typed edges and the contradictions between them (spec 017 B-4, B-6, B-7,
//! B-10).
//!
//! An edge is a derived routing hint. It names every memory it came from and
//! the extractor version that produced it, and it is never an answer in
//! itself (B-6). Re-extraction does not edit an edge in place: the prior
//! version's edges for that memory are superseded, so the history of what
//! the graph believed stays readable (B-7).

use core::fmt;

use aicortex_store::ReadStore;
use aicortex_types::MemoryId;
use rahi_store::{Statement, Value};
use rahi_types::{Error, UnixSeconds};
use serde::Deserialize;

use crate::entity::{EntityId, derived_id};
use crate::vocabulary::{EdgeStatus, Predicate};

/// The most edges one read returns.
pub const MAX_EDGES: u32 = 200;

/// The identity of one edge.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EdgeId(String);

impl EdgeId {
    /// Carry an identifier read back from storage.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// The identifier text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for EdgeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The object of an edge: another entity or a literal (B-4).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EntityRef {
    /// Another entity of the scope.
    Entity(EntityId),
    /// Literal text.
    Literal(String),
}

impl EntityRef {
    fn parts(&self) -> (Option<&str>, Option<&str>) {
        match self {
            Self::Entity(id) => (Some(id.as_str()), None),
            Self::Literal(text) => (None, Some(text.as_str())),
        }
    }

    fn key(&self) -> String {
        match self {
            Self::Entity(id) => format!("e:{id}"),
            Self::Literal(text) => format!("l:{text}"),
        }
    }
}

/// A typed edge (B-4).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edge {
    /// Its identity.
    pub id: EdgeId,
    /// The owning scope id.
    pub scope: String,
    /// Where it starts.
    pub subject: EntityId,
    /// The relation, from the closed vocabulary.
    pub predicate: Predicate,
    /// Where it ends.
    pub object: EntityRef,
    /// The memories it was derived from, sorted. Empty only once every one
    /// was erased, when the status is [`EdgeStatus::OriginErased`].
    pub derived_from: Vec<MemoryId>,
    /// The extractor's confidence in permille (0 to 1000).
    pub confidence: u16,
    /// The extractor's name.
    pub extractor: String,
    /// The extractor version that produced it.
    pub extractor_version: u32,
    /// When the fact began to hold, as far as the source says.
    pub valid_from: UnixSeconds,
    /// When it stopped holding, if it did.
    pub valid_until: Option<UnixSeconds>,
    /// Its standing.
    pub status: EdgeStatus,
}

/// Two active edges that disagree (B-10), surfaced and never resolved here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Contradiction {
    /// Its identity, derived from the pair.
    pub id: String,
    /// The lesser edge id.
    pub edge_a: EdgeId,
    /// The greater edge id.
    pub edge_b: EdgeId,
    /// The shared subject.
    pub subject: EntityId,
    /// The shared predicate.
    pub predicate: Predicate,
    /// When it was detected.
    pub detected: UnixSeconds,
    /// `open` until a review resolves it (spec 023).
    pub status: String,
}

#[derive(Debug, Deserialize)]
struct EdgeRow {
    edge_id: String,
    subject: String,
    predicate: String,
    object_entity: Option<String>,
    object_literal: Option<String>,
    confidence: i64,
    extractor: String,
    extractor_version: i64,
    valid_from: i64,
    valid_until: Option<i64>,
    status: String,
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
    subject: String,
    predicate: String,
    detected: i64,
    status: String,
}

fn unix(value: i64) -> UnixSeconds {
    UnixSeconds::new(u64::try_from(value).unwrap_or(0))
}

fn predicate_of(text: &str) -> Result<Predicate, Error> {
    Predicate::parse(text).ok_or_else(|| {
        Error::Integrity(format!(
            "stored predicate {text:?} is not in the vocabulary"
        ))
    })
}

async fn assemble(store: &impl ReadStore, scope_id: &str, row: EdgeRow) -> Result<Edge, Error> {
    let sources: Vec<SourceRow> = store
        .query(
            "SELECT memory_id FROM edge_source
             WHERE scope_id = ?1 AND edge_id = ?2 ORDER BY memory_id",
            vec![Value::from(scope_id), Value::from(row.edge_id.as_str())],
        )
        .await?;
    let object = match (row.object_entity, row.object_literal) {
        (Some(id), None) => EntityRef::Entity(EntityId::new(id)),
        (None, Some(text)) => EntityRef::Literal(text),
        _ => {
            return Err(Error::Integrity(format!(
                "edge {} has no single object",
                row.edge_id
            )));
        }
    };
    Ok(Edge {
        id: EdgeId::new(row.edge_id),
        scope: scope_id.to_owned(),
        subject: EntityId::new(row.subject),
        predicate: predicate_of(&row.predicate)?,
        object,
        derived_from: sources
            .into_iter()
            .map(|s| {
                MemoryId::parse(&s.memory_id)
                    .map_err(|e| Error::Integrity(format!("edge source id: {e}")))
            })
            .collect::<Result<_, _>>()?,
        confidence: u16::try_from(row.confidence).unwrap_or(0),
        extractor: row.extractor,
        extractor_version: u32::try_from(row.extractor_version).unwrap_or(0),
        valid_from: unix(row.valid_from),
        valid_until: row.valid_until.map(unix),
        status: EdgeStatus::parse(&row.status)
            .ok_or_else(|| Error::Integrity(format!("stored edge status {:?}", row.status)))?,
    })
}

/// Edges of a scope that start or end at `entity` or at an entity merged
/// into it, strongest first, at most `limit`.
///
/// # Errors
///
/// Store errors or stored values outside their vocabularies.
pub async fn edges_of(
    store: &impl ReadStore,
    scope_id: &str,
    entity: &EntityId,
    status: Option<EdgeStatus>,
    limit: u32,
) -> Result<Vec<Edge>, Error> {
    let rows: Vec<EdgeRow> = store
        .query(
            "SELECT edge_id, subject, predicate, object_entity, object_literal, confidence,
                    extractor, extractor_version, valid_from, valid_until, status
             FROM edge
             WHERE scope_id = ?1 AND (?3 = '' OR status = ?3)
               AND (subject IN (SELECT entity_id FROM entity WHERE scope_id = ?1
                                 AND (entity_id = ?2 OR merged_into = ?2))
                 OR object_entity IN (SELECT entity_id FROM entity WHERE scope_id = ?1
                                 AND (entity_id = ?2 OR merged_into = ?2)))
             ORDER BY confidence DESC, edge_id LIMIT ?4",
            vec![
                Value::from(scope_id),
                Value::from(entity.as_str()),
                Value::from(status.map_or("", EdgeStatus::label)),
                Value::Integer(i64::from(limit.clamp(1, MAX_EDGES))),
            ],
        )
        .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        out.push(assemble(store, scope_id, row).await?);
    }
    Ok(out)
}

/// The scope's open and closed contradictions, oldest first, at most `limit`.
///
/// # Errors
///
/// Store errors or stored values outside their vocabularies.
pub async fn contradictions(
    store: &impl ReadStore,
    scope_id: &str,
    limit: u32,
) -> Result<Vec<Contradiction>, Error> {
    let rows: Vec<ContradictionRow> = store
        .query(
            "SELECT contradiction_id, edge_a, edge_b, subject, predicate, detected, status
             FROM contradiction WHERE scope_id = ?1
             ORDER BY detected, contradiction_id LIMIT ?2",
            vec![
                Value::from(scope_id),
                Value::Integer(i64::from(limit.clamp(1, MAX_EDGES))),
            ],
        )
        .await?;
    rows.into_iter()
        .map(|row| {
            Ok(Contradiction {
                id: row.contradiction_id,
                edge_a: EdgeId::new(row.edge_a),
                edge_b: EdgeId::new(row.edge_b),
                subject: EntityId::new(row.subject),
                predicate: predicate_of(&row.predicate)?,
                detected: unix(row.detected),
                status: row.status,
            })
        })
        .collect()
}

/// One edge the extractor proposed and the vocabulary admitted, with its
/// entities resolved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlannedEdge {
    /// Where it starts.
    pub subject: EntityId,
    /// The relation.
    pub predicate: Predicate,
    /// Where it ends.
    pub object: EntityRef,
    /// Confidence in permille.
    pub confidence: u16,
}

/// What [`plan_edges`] needs to know about the run.
#[derive(Clone, Copy, Debug)]
pub struct EdgeRun<'a> {
    /// The scope id.
    pub scope_id: &'a str,
    /// The memory the edges are derived from.
    pub memory: MemoryId,
    /// The extractor's name.
    pub extractor: &'a str,
    /// The extractor's version.
    pub version: u32,
    /// The memory's time, which starts validity.
    pub valid_from: UnixSeconds,
    /// When the run commits.
    pub now: UnixSeconds,
}

#[derive(Debug, Deserialize)]
struct PriorRow {
    edge_id: String,
    sources: i64,
}

#[derive(Debug, Deserialize)]
struct FoundRow {
    edge_id: String,
}

fn seconds(at: UnixSeconds) -> i64 {
    i64::try_from(at.get()).unwrap_or(i64::MAX)
}

/// The statements that make a memory's edges those of `run.version` (B-7).
///
/// Edges the memory supported at an older version are superseded, or lose
/// the memory as a source when others support them too; each proposed edge
/// either gains the memory as a source of the active edge stating the same
/// fact, or becomes a new edge. A new edge is checked against its siblings
/// for contradiction (B-10). Nothing is deleted.
///
/// # Errors
///
/// Store errors.
pub async fn plan_edges(
    store: &impl ReadStore,
    run: &EdgeRun<'_>,
    planned: &[PlannedEdge],
) -> Result<Vec<Statement>, Error> {
    let memory = run.memory.to_string();
    let prior: Vec<PriorRow> = store
        .query_consistent(
            "SELECT e.edge_id AS edge_id,
                    (SELECT COUNT(*) FROM edge_source c
                     WHERE c.scope_id = ?1 AND c.edge_id = e.edge_id) AS sources
             FROM edge e JOIN edge_source s
               ON s.scope_id = e.scope_id AND s.edge_id = e.edge_id
             WHERE e.scope_id = ?1 AND s.memory_id = ?2
               AND e.status = 'active' AND e.extractor_version < ?3",
            vec![
                Value::from(run.scope_id),
                Value::from(memory.as_str()),
                Value::Integer(i64::from(run.version)),
            ],
        )
        .await?;
    let mut statements = Vec::new();
    let mut superseded = Vec::new();
    for edge in prior {
        if edge.sources <= 1 {
            statements.push(Statement::with_params(
                "UPDATE edge SET status = 'superseded' WHERE scope_id = ?1 AND edge_id = ?2",
                vec![
                    Value::from(run.scope_id),
                    Value::from(edge.edge_id.as_str()),
                ],
            ));
            superseded.push(edge.edge_id);
        } else {
            statements.push(Statement::with_params(
                "DELETE FROM edge_source WHERE scope_id = ?1 AND edge_id = ?2 AND memory_id = ?3",
                vec![
                    Value::from(run.scope_id),
                    Value::from(edge.edge_id.as_str()),
                    Value::from(memory.as_str()),
                ],
            ));
        }
    }
    let mut seen = std::collections::BTreeSet::new();
    for edge in planned {
        if !seen.insert((edge.subject.clone(), edge.predicate, edge.object.clone())) {
            continue;
        }
        let (entity, literal) = edge.object.parts();
        let found: Vec<FoundRow> = store
            .query_consistent(
                "SELECT edge_id FROM edge
                 WHERE scope_id = ?1 AND subject = ?2 AND predicate = ?3 AND status = 'active'
                   AND COALESCE(object_entity, '') = ?4 AND COALESCE(object_literal, '') = ?5",
                vec![
                    Value::from(run.scope_id),
                    Value::from(edge.subject.as_str()),
                    Value::from(edge.predicate.as_str()),
                    Value::from(entity.unwrap_or("")),
                    Value::from(literal.unwrap_or("")),
                ],
            )
            .await?;
        let reusable = found.into_iter().find(|f| !superseded.contains(&f.edge_id));
        if let Some(existing) = reusable {
            statements.push(source_row(run.scope_id, &existing.edge_id, &memory));
            continue;
        }
        let id = derived_id(
            "edg",
            &[
                run.scope_id,
                edge.subject.as_str(),
                edge.predicate.as_str(),
                &edge.object.key(),
                &memory,
                &run.version.to_string(),
            ],
        );
        statements.push(Statement::with_params(
            "INSERT INTO edge
             (edge_id, scope_id, subject, predicate, object_entity, object_literal, confidence,
              extractor, extractor_version, valid_from, status, created)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 'active', ?11)
             ON CONFLICT (edge_id) DO NOTHING",
            vec![
                Value::from(id.as_str()),
                Value::from(run.scope_id),
                Value::from(edge.subject.as_str()),
                Value::from(edge.predicate.as_str()),
                Value::from(entity.map(str::to_owned)),
                Value::from(literal.map(str::to_owned)),
                Value::Integer(i64::from(edge.confidence)),
                Value::from(run.extractor),
                Value::Integer(i64::from(run.version)),
                Value::Integer(seconds(run.valid_from)),
                Value::Integer(seconds(run.now)),
            ],
        ));
        statements.push(source_row(run.scope_id, &id, &memory));
        statements.push(Statement::with_params(
            "INSERT INTO contradiction
               (contradiction_id, scope_id, edge_a, edge_b, subject, predicate, detected)
             SELECT 'ctd-' || MIN(n.edge_id, o.edge_id) || '-' || MAX(n.edge_id, o.edge_id),
                    n.scope_id, MIN(n.edge_id, o.edge_id), MAX(n.edge_id, o.edge_id),
                    n.subject, n.predicate, ?3
             FROM edge n JOIN edge o
               ON o.scope_id = n.scope_id AND o.subject = n.subject
              AND o.predicate = n.predicate AND o.edge_id <> n.edge_id AND o.status = 'active'
             WHERE n.scope_id = ?1 AND n.edge_id = ?2 AND n.status = 'active'
               AND (COALESCE(o.object_entity, '') <> COALESCE(n.object_entity, '')
                 OR COALESCE(o.object_literal, '') <> COALESCE(n.object_literal, ''))
               AND COALESCE(o.valid_until, 9223372036854775807) > n.valid_from
               AND COALESCE(n.valid_until, 9223372036854775807) > o.valid_from
             ON CONFLICT DO NOTHING",
            vec![
                Value::from(run.scope_id),
                Value::from(id.as_str()),
                Value::Integer(seconds(run.now)),
            ],
        ));
    }
    Ok(statements)
}

fn source_row(scope_id: &str, edge_id: &str, memory: &str) -> Statement {
    Statement::with_params(
        "INSERT INTO edge_source (scope_id, edge_id, memory_id) VALUES (?1, ?2, ?3)
         ON CONFLICT DO NOTHING",
        vec![
            Value::from(scope_id),
            Value::from(edge_id),
            Value::from(memory),
        ],
    )
}
