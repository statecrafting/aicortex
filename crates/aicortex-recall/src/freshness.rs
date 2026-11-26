//! Freshness signals (spec 018 B-11, FR-008, FR-009).
//!
//! Per resolved entity and per source, the capture time of the newest active
//! memory and a count, in the caller's scope, ignoring the query's time
//! range, read at the injected clock. Ids, kinds, times, and counts only: no
//! body, entity name, alias, or sentence, so the block stays outside spec
//! 019's envelope.

use aicortex_graph::EntityId;
use aicortex_store::ScopeId;
use aicortex_types::MemoryId;
use rahi_store::{StoreHandle, Value};
use rahi_types::{Error, UnixSeconds};
use serde::{Deserialize, Serialize};

use crate::channels::json_ids;

/// The newest memory that mentions a resolved entity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct EntityFreshness {
    /// The entity.
    pub entity_id: String,
    /// The newest active mentioning memory's capture time.
    pub newest: UnixSeconds,
    /// How many active memories mention it.
    pub count: u64,
}

/// The newest memory from a source.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SourceFreshness {
    /// The source system.
    pub system: String,
    /// The source's own reference for the item, when it has one.
    pub reference: Option<String>,
    /// The newest active memory's capture time.
    pub newest: UnixSeconds,
    /// How many active memories came from it.
    pub count: u64,
}

/// The block of B-11.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Freshness {
    /// Per entity resolved from the query, newest first then id.
    pub entities: Vec<EntityFreshness>,
    /// Whether the entity list was cut.
    pub entities_truncated: bool,
    /// Per source among the results and the filter, newest first then key.
    pub sources: Vec<SourceFreshness>,
    /// Whether the source list was cut.
    pub sources_truncated: bool,
}

#[derive(Debug, Deserialize)]
struct EntityRow {
    newest: Option<i64>,
    count: i64,
}

#[derive(Debug, Deserialize)]
struct SourceKeyRow {
    system: String,
    reference: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SourceRow {
    newest: Option<i64>,
    count: i64,
}

fn seconds(at: UnixSeconds) -> i64 {
    i64::try_from(at.get()).unwrap_or(i64::MAX)
}

fn time(value: i64) -> UnixSeconds {
    UnixSeconds::new(u64::try_from(value).unwrap_or(0))
}

/// Compute the block. `returned` are the result's memories; `sources` are
/// the systems the filter names.
///
/// # Errors
///
/// Store errors.
pub async fn compute(
    store: &StoreHandle,
    scope: &ScopeId,
    entities: &[EntityId],
    returned: &[MemoryId],
    sources: &[String],
    now: UnixSeconds,
    limit: usize,
) -> Result<Freshness, Error> {
    let mut block = Freshness::default();
    for entity in entities {
        let rows: Vec<EntityRow> = store
            .query(
                "SELECT MAX(p.captured_at) AS newest, COUNT(*) AS count
                 FROM memory m JOIN provenance p
                   ON p.memory_id = m.id AND p.scope_id = m.scope_id
                 WHERE m.scope_id = ?1 AND m.status = 'active' AND p.captured_at <= ?3
                   AND m.id IN (SELECT n.memory_id FROM mention n
                     WHERE n.scope_id = ?1 AND n.entity_id IN
                       (SELECT entity_id FROM entity
                        WHERE scope_id = ?1 AND (entity_id = ?2 OR merged_into = ?2)))"
                    .to_owned(),
                vec![
                    Value::from(scope.as_str()),
                    Value::from(entity.as_str()),
                    Value::Integer(seconds(now)),
                ],
            )
            .await?;
        if let Some(EntityRow {
            newest: Some(newest),
            count,
        }) = rows.into_iter().next()
            && count > 0
        {
            block.entities.push(EntityFreshness {
                entity_id: entity.as_str().to_owned(),
                newest: time(newest),
                count: u64::try_from(count).unwrap_or(0),
            });
        }
    }
    block.entities.sort_by(|a, b| {
        b.newest
            .cmp(&a.newest)
            .then_with(|| a.entity_id.cmp(&b.entity_id))
    });
    block.entities_truncated = block.entities.len() > limit;
    block.entities.truncate(limit);

    // The source keys: those of the returned memories, and the named systems.
    let mut keys: Vec<(String, Option<String>)> = Vec::new();
    if !returned.is_empty() {
        let ids: Vec<String> = returned.iter().map(ToString::to_string).collect();
        let rows: Vec<SourceKeyRow> = store
            .query(
                "SELECT DISTINCT source_system AS system, source_external_id AS reference
                 FROM provenance WHERE scope_id = ?1
                   AND memory_id IN (SELECT value FROM json_each(?2))"
                    .to_owned(),
                vec![Value::from(scope.as_str()), Value::from(json_ids(&ids))],
            )
            .await?;
        keys.extend(rows.into_iter().map(|row| (row.system, row.reference)));
    }
    for system in sources {
        let rows: Vec<SourceKeyRow> = store
            .query(
                "SELECT DISTINCT source_system AS system, source_external_id AS reference
                 FROM provenance WHERE scope_id = ?1 AND source_system = ?2"
                    .to_owned(),
                vec![Value::from(scope.as_str()), Value::from(system.as_str())],
            )
            .await?;
        keys.extend(rows.into_iter().map(|row| (row.system, row.reference)));
    }
    keys.sort();
    keys.dedup();
    for (system, reference) in keys {
        let rows: Vec<SourceRow> = store
            .query(
                "SELECT MAX(p.captured_at) AS newest, COUNT(*) AS count
                 FROM provenance p JOIN memory m
                   ON m.id = p.memory_id AND m.scope_id = p.scope_id
                 WHERE p.scope_id = ?1 AND m.status = 'active' AND p.captured_at <= ?4
                   AND p.source_system = ?2 AND COALESCE(p.source_external_id, '') = ?3"
                    .to_owned(),
                vec![
                    Value::from(scope.as_str()),
                    Value::from(system.as_str()),
                    Value::from(reference.as_deref().unwrap_or("")),
                    Value::Integer(seconds(now)),
                ],
            )
            .await?;
        if let Some(SourceRow {
            newest: Some(newest),
            count,
        }) = rows.into_iter().next()
            && count > 0
        {
            block.sources.push(SourceFreshness {
                system,
                reference,
                newest: time(newest),
                count: u64::try_from(count).unwrap_or(0),
            });
        }
    }
    block.sources.sort_by(|a, b| {
        b.newest
            .cmp(&a.newest)
            .then_with(|| a.system.cmp(&b.system))
            .then_with(|| a.reference.cmp(&b.reference))
    });
    block.sources_truncated = block.sources.len() > limit;
    block.sources.truncate(limit);
    Ok(block)
}
