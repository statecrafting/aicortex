//! Entities and their aliases (spec 017 B-1, B-3).
//!
//! An entity belongs to exactly one scope. Every row is derived: it is
//! created by resolving a mention found in a memory, and it is never
//! authoritative about what that memory said (B-6). A merged entity is kept
//! and points at its survivor, so a merge is reversible (B-2); every read
//! here follows that pointer one hop and presents the survivor with the
//! absorbed entity's aliases and mentions folded in.

use core::fmt;

use aicortex_store::ReadStore;
use rahi_store::{Statement, TxnBuilder, Value};
use rahi_types::{Error, UnixSeconds};
use ring::digest::{SHA256, digest};
use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;

use crate::resolve::ContextVector;
use crate::vocabulary::EntityKind;

/// The most entities one listing returns.
pub const MAX_LIST: u32 = 100;

/// The identity of one entity: an opaque, deterministic token.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EntityId(String);

impl EntityId {
    /// Carry an identifier read back from storage or a request.
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

impl fmt::Display for EntityId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A resolved entity (B-1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entity {
    /// Its identity.
    pub id: EntityId,
    /// The scope it belongs to, as the store's scope id.
    pub scope: String,
    /// What it is.
    pub kind: EntityKind,
    /// The name it is shown under.
    pub canonical: String,
    /// Every other name it has been seen under, sorted.
    pub aliases: Vec<String>,
    /// The earliest moment a memory mentioned it.
    pub first_seen: UnixSeconds,
    /// The latest moment a memory mentioned it.
    pub last_seen: UnixSeconds,
    /// How many mentions currently point at it.
    pub mention_count: u64,
}

/// The comparison form of a surface string (B-2): compatibility-folded,
/// lowercased, punctuation read as a separator, whitespace collapsed.
#[must_use]
pub fn normalize(surface: &str) -> String {
    let folded: String = surface
        .nfkc()
        .flat_map(char::to_lowercase)
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect();
    folded.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A deterministic identifier: the SHA-256 of `parts`, framed so two
/// different part lists never collide, shortened to 128 bits.
///
/// Determinism is what makes a retried extraction commit idempotent: the
/// same mention mints the same entity id and the same edge mints the same
/// edge id.
#[must_use]
pub fn derived_id(prefix: &str, parts: &[&str]) -> String {
    let mut material = Vec::new();
    for part in parts {
        material.extend_from_slice(&u64::try_from(part.len()).unwrap_or(u64::MAX).to_le_bytes());
        material.extend_from_slice(part.as_bytes());
    }
    let hash = digest(&SHA256, &material);
    let mut out = String::from(prefix);
    out.push('-');
    for byte in hash.as_ref().iter().take(16) {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

#[derive(Debug, Deserialize)]
struct EntityRow {
    entity_id: String,
    kind: String,
    canonical: String,
    merged_into: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SpanRow {
    first_seen: i64,
    last_seen: i64,
    mentions: i64,
}

#[derive(Debug, Deserialize)]
struct AliasRow {
    alias: String,
}

#[derive(Debug, Deserialize)]
struct IdRow {
    entity_id: String,
}

fn unix(value: i64) -> UnixSeconds {
    UnixSeconds::new(u64::try_from(value).unwrap_or(0))
}

fn kind_of(label: &str) -> Result<EntityKind, Error> {
    EntityKind::parse(label).ok_or_else(|| {
        Error::Integrity(format!(
            "stored entity kind {label:?} is not in the vocabulary"
        ))
    })
}

/// One entity of a scope, presented as its survivor if it was merged.
///
/// # Errors
///
/// Store errors or a stored kind outside the vocabulary.
pub async fn get(
    store: &impl ReadStore,
    scope_id: &str,
    id: &EntityId,
) -> Result<Option<Entity>, Error> {
    let rows: Vec<EntityRow> = store
        .query(
            "SELECT entity_id, kind, canonical, merged_into FROM entity
             WHERE scope_id = ?1 AND entity_id = ?2",
            vec![Value::from(scope_id), Value::from(id.as_str())],
        )
        .await?;
    let Some(mut row) = rows.into_iter().next() else {
        return Ok(None);
    };
    if let Some(survivor) = row.merged_into.take() {
        let rows: Vec<EntityRow> = store
            .query(
                "SELECT entity_id, kind, canonical, merged_into FROM entity
                 WHERE scope_id = ?1 AND entity_id = ?2",
                vec![Value::from(scope_id), Value::from(survivor)],
            )
            .await?;
        let Some(found) = rows.into_iter().next() else {
            return Ok(None);
        };
        row = found;
    }
    let cluster = vec![Value::from(scope_id), Value::from(row.entity_id.as_str())];
    let spans: Vec<SpanRow> = store
        .query(
            "SELECT
               (SELECT MIN(first_seen) FROM entity WHERE scope_id = ?1
                  AND (entity_id = ?2 OR merged_into = ?2)) AS first_seen,
               (SELECT MAX(last_seen) FROM entity WHERE scope_id = ?1
                  AND (entity_id = ?2 OR merged_into = ?2)) AS last_seen,
               (SELECT COUNT(*) FROM mention WHERE scope_id = ?1 AND entity_id IN
                  (SELECT entity_id FROM entity WHERE scope_id = ?1
                     AND (entity_id = ?2 OR merged_into = ?2))) AS mentions",
            cluster.clone(),
        )
        .await?;
    let span = spans
        .into_iter()
        .next()
        .ok_or_else(|| Error::Integrity("entity span returned no row".to_owned()))?;
    let aliases: Vec<AliasRow> = store
        .query(
            "SELECT DISTINCT alias FROM entity_alias
             WHERE scope_id = ?1 AND entity_id IN
               (SELECT entity_id FROM entity WHERE scope_id = ?1
                  AND (entity_id = ?2 OR merged_into = ?2))
             ORDER BY alias",
            cluster,
        )
        .await?;
    Ok(Some(Entity {
        id: EntityId::new(row.entity_id),
        scope: scope_id.to_owned(),
        kind: kind_of(&row.kind)?,
        aliases: aliases
            .into_iter()
            .map(|a| a.alias)
            .filter(|alias| *alias != row.canonical)
            .collect(),
        canonical: row.canonical,
        first_seen: unix(span.first_seen),
        last_seen: unix(span.last_seen),
        mention_count: u64::try_from(span.mentions).unwrap_or(0),
    }))
}

/// A page of a scope's entities (merged ones are shown under their survivor),
/// in id order, strictly after `after`.
///
/// # Errors
///
/// Store errors or a stored kind outside the vocabulary.
pub async fn list(
    store: &impl ReadStore,
    scope_id: &str,
    kind: Option<EntityKind>,
    after: Option<&str>,
    limit: u32,
) -> Result<Vec<Entity>, Error> {
    let limit = limit.clamp(1, MAX_LIST);
    let ids: Vec<IdRow> = store
        .query(
            "SELECT entity_id FROM entity
             WHERE scope_id = ?1 AND merged_into IS NULL AND entity_id > ?2
               AND (?3 = '' OR kind = ?3)
             ORDER BY entity_id LIMIT ?4",
            vec![
                Value::from(scope_id),
                Value::from(after.unwrap_or("")),
                Value::from(kind.map_or("", EntityKind::label)),
                Value::Integer(i64::from(limit)),
            ],
        )
        .await?;
    let mut out = Vec::with_capacity(ids.len());
    for row in ids {
        if let Some(entity) = get(store, scope_id, &EntityId::new(row.entity_id)).await? {
            out.push(entity);
        }
    }
    Ok(out)
}

/// What a new entity row needs.
#[derive(Clone, Debug)]
pub struct NewEntity<'a> {
    /// The deterministic identity.
    pub id: &'a EntityId,
    /// The owning scope id.
    pub scope_id: &'a str,
    /// The kind.
    pub kind: EntityKind,
    /// The name shown.
    pub canonical: &'a str,
    /// The memory's time.
    pub seen: UnixSeconds,
    /// The mention's context vector, which names its model (B-2).
    pub vector: Option<&'a ContextVector>,
}

fn seconds(at: UnixSeconds) -> i64 {
    i64::try_from(at.get()).unwrap_or(i64::MAX)
}

/// Stage a new entity and its canonical alias. A repeat is a no-op.
pub fn stage_entity(txn: &mut TxnBuilder, new: &NewEntity<'_>) {
    let (model, revision, dims, normalized, blob) = new.vector.map_or(
        (
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
        ),
        |v| {
            (
                Value::from(v.model_id.as_str()),
                Value::Integer(i64::from(v.revision)),
                Value::Integer(i64::from(v.dims)),
                Value::from(v.normalized),
                Value::Blob(v.values.iter().flat_map(|x| x.to_le_bytes()).collect()),
            )
        },
    );
    txn.push(Statement::with_params(
        "INSERT INTO entity
         (entity_id, scope_id, kind, canonical, first_seen, last_seen,
          vector_model, vector_revision, vector_dims, vector_normalized, vector)
         VALUES (?1, ?2, ?3, ?4, ?5, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT (entity_id) DO NOTHING",
        vec![
            Value::from(new.id.as_str()),
            Value::from(new.scope_id),
            Value::from(new.kind.label()),
            Value::from(new.canonical),
            Value::Integer(seconds(new.seen)),
            model,
            revision,
            dims,
            normalized,
            blob,
        ],
    ));
    stage_alias(txn, new.scope_id, new.id, new.canonical);
}

/// Stage an alias for an entity. A repeat is a no-op.
pub fn stage_alias(txn: &mut TxnBuilder, scope_id: &str, entity: &EntityId, alias: &str) {
    txn.push(Statement::with_params(
        "INSERT INTO entity_alias (scope_id, entity_id, alias, normalized)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT (scope_id, entity_id, alias) DO NOTHING",
        vec![
            Value::from(scope_id),
            Value::from(entity.as_str()),
            Value::from(alias),
            Value::from(normalize(alias)),
        ],
    ));
}

/// Stage a widening of an entity's seen window.
pub fn stage_seen(txn: &mut TxnBuilder, scope_id: &str, entity: &EntityId, seen: UnixSeconds) {
    txn.push(Statement::with_params(
        "UPDATE entity SET first_seen = MIN(first_seen, ?3), last_seen = MAX(last_seen, ?3)
         WHERE scope_id = ?1 AND entity_id = ?2",
        vec![
            Value::from(scope_id),
            Value::from(entity.as_str()),
            Value::Integer(seconds(seen)),
        ],
    ));
}
