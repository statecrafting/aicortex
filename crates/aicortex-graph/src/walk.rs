//! The bounded neighbor walk (spec 017 B-9, B-6).
//!
//! `neighbors` follows active edges outward from an entity for at most
//! `depth` hops and at most `fanout` edges per node per hop, and returns the
//! memories those edges were derived from, each with the path that reached
//! it. The path is part of the recall trace. A walk terminates on a cyclic
//! graph because each entity is expanded once. It returns identifiers only:
//! the memories carry their own trust class, and content leaves through the
//! untrusted-content boundary of spec 019, never through here (B-6).

use std::collections::{BTreeMap, BTreeSet};

use aicortex_store::ReadStore;
use aicortex_types::MemoryId;
use rahi_store::Value;
use rahi_types::Error;
use serde::Deserialize;

use crate::edge::EdgeId;
use crate::entity::EntityId;
use crate::vocabulary::Predicate;

/// The depth a walk takes when none is asked for.
pub const DEFAULT_DEPTH: u8 = 2;
/// The deepest walk allowed.
pub const MAX_DEPTH: u8 = 4;
/// The edges followed per node per hop when none is asked for.
pub const DEFAULT_FANOUT: u16 = 16;
/// The widest fan-out allowed.
pub const MAX_FANOUT: u16 = 64;
/// The most memories one walk returns.
pub const MAX_MEMORIES: usize = 200;

/// Which edges a walk may follow.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WalkFilter {
    /// Only these predicates; all of them when `None`.
    pub predicates: Option<BTreeSet<Predicate>>,
    /// Only edges at or above this confidence, in permille.
    pub min_confidence: u16,
}

/// The bounds of one walk. Out-of-range values are clamped, never refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WalkOptions {
    /// Hops to follow, `1..=MAX_DEPTH`.
    pub depth: u8,
    /// Edges followed per node per hop, `1..=MAX_FANOUT`.
    pub fanout: u16,
    /// The edge filter.
    pub filter: WalkFilter,
}

impl Default for WalkOptions {
    fn default() -> Self {
        Self {
            depth: DEFAULT_DEPTH,
            fanout: DEFAULT_FANOUT,
            filter: WalkFilter::default(),
        }
    }
}

/// Where a hop arrived.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HopEnd {
    /// Another entity.
    Entity(EntityId),
    /// A literal object; the walk goes no further from it.
    Literal(String),
}

/// One step of a path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hop {
    /// The entity the hop left.
    pub from: EntityId,
    /// The edge followed.
    pub edge: EdgeId,
    /// Its predicate.
    pub predicate: Predicate,
    /// Where it arrived.
    pub to: HopEnd,
}

/// The hops from the start entity to the edge a memory was derived from.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Path(pub Vec<Hop>);

/// A memory a walk reached and the shortest path that reached it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WalkHit {
    /// The memory.
    pub memory: MemoryId,
    /// How the walk got to it.
    pub path: Path,
}

#[derive(Debug, Deserialize)]
struct StartRow {
    entity_id: String,
}

#[derive(Debug, Deserialize)]
struct HopRow {
    edge_id: String,
    predicate: String,
    object_literal: Option<String>,
    subject_canon: Option<String>,
    object_canon: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SourceRow {
    memory_id: String,
}

/// Walk outward from `entity` (B-9).
///
/// An entity that does not exist in the scope yields no hits. Quarantined
/// and erased memories are never returned.
///
/// # Errors
///
/// Store errors or stored values outside their vocabularies.
pub async fn neighbors(
    store: &impl ReadStore,
    scope_id: &str,
    entity: &EntityId,
    options: &WalkOptions,
) -> Result<Vec<WalkHit>, Error> {
    let depth = options.depth.clamp(1, MAX_DEPTH);
    let fanout = options.fanout.clamp(1, MAX_FANOUT);
    let predicates = options
        .filter
        .predicates
        .as_ref()
        .map_or_else(String::new, |set| {
            let mut text = String::from("|");
            for predicate in set {
                text.push_str(predicate.as_str());
                text.push('|');
            }
            text
        });
    let start: Vec<StartRow> = store
        .query(
            "SELECT COALESCE(merged_into, entity_id) AS entity_id FROM entity
             WHERE scope_id = ?1 AND entity_id = ?2",
            vec![Value::from(scope_id), Value::from(entity.as_str())],
        )
        .await?;
    let Some(start) = start.into_iter().next() else {
        return Ok(Vec::new());
    };
    let start = EntityId::new(start.entity_id);
    let mut visited = BTreeSet::from([start.clone()]);
    let mut frontier = vec![(start, Vec::<Hop>::new())];
    let mut hits: BTreeMap<MemoryId, Path> = BTreeMap::new();
    let mut order = Vec::new();
    for _ in 0..depth {
        let mut next = Vec::new();
        for (node, trail) in &frontier {
            let rows: Vec<HopRow> = store
                .query(
                    "SELECT e.edge_id AS edge_id, e.predicate AS predicate,
                            e.object_literal AS object_literal,
                            (SELECT COALESCE(s.merged_into, s.entity_id) FROM entity s
                              WHERE s.scope_id = ?1 AND s.entity_id = e.subject) AS subject_canon,
                            (SELECT COALESCE(o.merged_into, o.entity_id) FROM entity o
                              WHERE o.scope_id = ?1 AND o.entity_id = e.object_entity) AS object_canon
                     FROM edge e
                     WHERE e.scope_id = ?1 AND e.status = 'active' AND e.confidence >= ?3
                       AND (?5 = '' OR instr(?5, '|' || e.predicate || '|') > 0)
                       AND (e.subject IN (SELECT entity_id FROM entity WHERE scope_id = ?1
                                           AND (entity_id = ?2 OR merged_into = ?2))
                         OR e.object_entity IN (SELECT entity_id FROM entity WHERE scope_id = ?1
                                           AND (entity_id = ?2 OR merged_into = ?2)))
                     ORDER BY e.confidence DESC, e.edge_id LIMIT ?4",
                    vec![
                        Value::from(scope_id),
                        Value::from(node.as_str()),
                        Value::Integer(i64::from(options.filter.min_confidence)),
                        Value::Integer(i64::from(fanout)),
                        Value::from(predicates.as_str()),
                    ],
                )
                .await?;
            for row in rows {
                let predicate = Predicate::parse(&row.predicate).ok_or_else(|| {
                    Error::Integrity(format!("stored predicate {:?}", row.predicate))
                })?;
                let subject = row.subject_canon.map(EntityId::new);
                let object = row.object_canon.map(EntityId::new);
                let to = if subject.as_ref() == Some(node) {
                    match (object, row.object_literal) {
                        (Some(id), _) => HopEnd::Entity(id),
                        (None, Some(text)) => HopEnd::Literal(text),
                        (None, None) => continue,
                    }
                } else {
                    match subject {
                        Some(id) => HopEnd::Entity(id),
                        None => continue,
                    }
                };
                let mut path = trail.clone();
                path.push(Hop {
                    from: node.clone(),
                    edge: EdgeId::new(row.edge_id.as_str()),
                    predicate,
                    to: to.clone(),
                });
                let sources: Vec<SourceRow> = store
                    .query(
                        "SELECT s.memory_id AS memory_id FROM edge_source s
                         JOIN memory m ON m.scope_id = s.scope_id AND m.id = s.memory_id
                           AND m.status NOT IN ('erased', 'quarantined')
                         WHERE s.scope_id = ?1 AND s.edge_id = ?2 ORDER BY s.memory_id",
                        vec![Value::from(scope_id), Value::from(row.edge_id.as_str())],
                    )
                    .await?;
                for source in sources {
                    let id = MemoryId::parse(&source.memory_id)
                        .map_err(|e| Error::Integrity(format!("edge source id: {e}")))?;
                    if hits.len() >= MAX_MEMORIES {
                        break;
                    }
                    if let std::collections::btree_map::Entry::Vacant(slot) = hits.entry(id) {
                        slot.insert(Path(path.clone()));
                        order.push(id);
                    }
                }
                if let HopEnd::Entity(id) = to
                    && visited.insert(id.clone())
                {
                    next.push((id, path));
                }
            }
        }
        if next.is_empty() {
            break;
        }
        frontier = next;
    }
    Ok(order
        .into_iter()
        .filter_map(|memory| hits.remove(&memory).map(|path| WalkHit { memory, path }))
        .collect())
}
