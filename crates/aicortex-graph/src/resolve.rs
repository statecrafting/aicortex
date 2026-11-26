//! Resolving a mention to an entity, and merging entities (spec 017 B-2).
//!
//! A mention resolves by exact alias match, then by normalized-form match,
//! then by embedding similarity above a high threshold within the same kind.
//! Below the threshold a new entity is created. All three steps stay inside
//! one scope and one kind, and the third compares only against vectors of
//! the mention's own model, revision, width, and normalization.
//!
//! Resolution never destroys: a merge marks the absorbed entity and records
//! both ids, and can be reversed for a retention window.

use aicortex_index::{decode, dot};
use aicortex_store::ReadStore;
use rahi_store::{Blob, Statement, StoreHandle, Value};
use rahi_types::{Error, UnixSeconds};
use serde::Deserialize;

use crate::entity::{EntityId, derived_id, normalize};
use crate::vocabulary::EntityKind;

/// The cosine similarity at or above which two mentions are one entity (B-2).
///
/// Deliberately high: a wrong merge hides a person, a missed merge only
/// leaves two records an operator can merge (D-3).
pub const SIMILARITY_THRESHOLD: f32 = 0.92;

/// The most candidate vectors one resolution compares against.
pub const MAX_CANDIDATES: i64 = 5000;

/// How long a merge stays reversible, in seconds (B-2): thirty days.
pub const MERGE_RETENTION_SECONDS: u64 = 30 * 24 * 60 * 60;

/// The vector of the text around a mention, with the identity of the model
/// that produced it.
#[derive(Clone, Debug, PartialEq)]
pub struct ContextVector {
    /// The model that embedded it.
    pub model_id: String,
    /// The model configuration revision.
    pub revision: u32,
    /// The width.
    pub dims: u16,
    /// Whether the model's vectors are unit length.
    pub normalized: bool,
    /// The values.
    pub values: Vec<f32>,
}

/// What resolution is asked about.
#[derive(Clone, Copy, Debug)]
pub struct MentionRef<'a> {
    /// The surface text.
    pub surface: &'a str,
    /// The kind the extractor assigned.
    pub kind: EntityKind,
    /// The embedding of the chunk around the mention, if one exists yet.
    pub context: Option<&'a ContextVector>,
}

/// Which step matched.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Via {
    /// An alias equal to the surface.
    Alias,
    /// An alias with the same normalized form.
    Normalized,
    /// A vector at or above the threshold.
    Embedding,
}

/// The outcome of resolution.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Resolution {
    /// The mention names this existing entity.
    Existing {
        /// The surviving entity's id.
        entity: EntityId,
        /// The step that matched.
        via: Via,
    },
    /// Nothing matched; the caller creates an entity.
    New,
}

#[derive(Debug, Deserialize)]
struct Matched {
    entity_id: String,
}

#[derive(Debug, Deserialize)]
struct Candidate {
    entity_id: String,
    vector: Blob,
}

/// Resolve one mention within `scope_id` (B-2).
///
/// # Errors
///
/// Store errors, or a stored vector that does not decode at its recorded
/// width.
pub async fn resolve(
    store: &impl ReadStore,
    scope_id: &str,
    mention: &MentionRef<'_>,
) -> Result<Resolution, Error> {
    let kind = Value::from(mention.kind.label());
    let exact: Vec<Matched> = store
        .query(
            "SELECT COALESCE(e.merged_into, e.entity_id) AS entity_id
             FROM entity_alias a JOIN entity e
               ON e.scope_id = a.scope_id AND e.entity_id = a.entity_id
             WHERE a.scope_id = ?1 AND a.alias = ?2 AND e.kind = ?3
             ORDER BY e.first_seen, e.entity_id LIMIT 1",
            vec![
                Value::from(scope_id),
                Value::from(mention.surface),
                kind.clone(),
            ],
        )
        .await?;
    if let Some(found) = exact.into_iter().next() {
        return Ok(existing(found, Via::Alias));
    }
    let normalized: Vec<Matched> = store
        .query(
            "SELECT COALESCE(e.merged_into, e.entity_id) AS entity_id
             FROM entity_alias a JOIN entity e
               ON e.scope_id = a.scope_id AND e.entity_id = a.entity_id
             WHERE a.scope_id = ?1 AND a.normalized = ?2 AND e.kind = ?3
             ORDER BY e.first_seen, e.entity_id LIMIT 1",
            vec![
                Value::from(scope_id),
                Value::from(normalize(mention.surface)),
                kind.clone(),
            ],
        )
        .await?;
    if let Some(found) = normalized.into_iter().next() {
        return Ok(existing(found, Via::Normalized));
    }
    let Some(context) = mention.context else {
        return Ok(Resolution::New);
    };
    let rows: Vec<Candidate> = store
        .query(
            "SELECT entity_id, vector FROM entity
             WHERE scope_id = ?1 AND kind = ?2 AND merged_into IS NULL
               AND vector_model = ?3 AND vector_revision = ?4
               AND vector_dims = ?5 AND vector_normalized = ?6
             ORDER BY first_seen, entity_id LIMIT ?7",
            vec![
                Value::from(scope_id),
                kind,
                Value::from(context.model_id.as_str()),
                Value::Integer(i64::from(context.revision)),
                Value::Integer(i64::from(context.dims)),
                Value::from(context.normalized),
                Value::Integer(MAX_CANDIDATES),
            ],
        )
        .await?;
    let mut best: Option<(f32, String)> = None;
    for row in rows {
        let vector = decode(row.vector.as_slice(), context.dims)
            .map_err(|e| Error::Integrity(format!("entity {} vector: {e}", row.entity_id)))?;
        let score = dot(&context.values, &vector);
        if score >= SIMILARITY_THRESHOLD && best.as_ref().is_none_or(|(top, _)| score > *top) {
            best = Some((score, row.entity_id));
        }
    }
    Ok(
        best.map_or(Resolution::New, |(_, id)| Resolution::Existing {
            entity: EntityId::new(id),
            via: Via::Embedding,
        }),
    )
}

fn existing(found: Matched, via: Via) -> Resolution {
    Resolution::Existing {
        entity: EntityId::new(found.entity_id),
        via,
    }
}

#[derive(Debug, Deserialize)]
struct StateRow {
    merged_into: Option<String>,
    absorbed_children: i64,
}

#[derive(Debug, Deserialize)]
struct MergeRow {
    survivor: String,
    absorbed: String,
    retain_until: i64,
    reversed_at: Option<i64>,
}

fn seconds(at: UnixSeconds) -> i64 {
    i64::try_from(at.get()).unwrap_or(i64::MAX)
}

/// Merge `absorbed` into `survivor` (B-2). Explicit, never automatic.
///
/// Both ids are recorded in `entity_merge`, nothing is deleted, and the
/// merge can be reversed by [`unmerge`] until the retention window closes.
/// Returns the merge id.
///
/// # Errors
///
/// [`Error::Validation`] when an entity is missing, the two are the same, or
/// either is already merged or has absorbed another; store errors.
pub async fn merge(
    store: &StoreHandle,
    scope_id: &str,
    survivor: &EntityId,
    absorbed: &EntityId,
    now: UnixSeconds,
) -> Result<String, Error> {
    if survivor == absorbed {
        return Err(Error::Validation(
            "an entity cannot be merged into itself".to_owned(),
        ));
    }
    for id in [survivor, absorbed] {
        let rows: Vec<StateRow> = store
            .query_consistent(
                "SELECT merged_into,
                        (SELECT COUNT(*) FROM entity c
                         WHERE c.scope_id = ?1 AND c.merged_into = ?2) AS absorbed_children
                 FROM entity WHERE scope_id = ?1 AND entity_id = ?2",
                vec![Value::from(scope_id), Value::from(id.as_str())],
            )
            .await?;
        let state = rows
            .into_iter()
            .next()
            .ok_or_else(|| Error::Validation(format!("entity {id} is not in this scope")))?;
        if state.merged_into.is_some() {
            return Err(Error::Validation(format!("entity {id} is already merged")));
        }
        if id == absorbed && state.absorbed_children > 0 {
            return Err(Error::Validation(format!(
                "entity {id} has absorbed others; unmerge them first"
            )));
        }
    }
    let merge_id = derived_id(
        "mrg",
        &[
            scope_id,
            survivor.as_str(),
            absorbed.as_str(),
            &now.get().to_string(),
        ],
    );
    // A violated precondition inserts a NULL into a NOT NULL column, which
    // rolls the whole transaction back.
    let guard = |entity: &EntityId| {
        Statement::with_params(
            "INSERT INTO entity_merge (merge_id, scope_id, survivor, absorbed, merged_at, retain_until)
             SELECT 'guard', NULL, '', '', 0, 0 WHERE NOT EXISTS (
               SELECT 1 FROM entity WHERE scope_id = ?1 AND entity_id = ?2 AND merged_into IS NULL)",
            vec![Value::from(scope_id), Value::from(entity.as_str())],
        )
    };
    store
        .txn(vec![
            guard(survivor),
            guard(absorbed),
            Statement::with_params(
                "UPDATE entity SET merged_into = ?3, merged_at = ?4
                 WHERE scope_id = ?1 AND entity_id = ?2 AND merged_into IS NULL",
                vec![
                    Value::from(scope_id),
                    Value::from(absorbed.as_str()),
                    Value::from(survivor.as_str()),
                    Value::Integer(seconds(now)),
                ],
            ),
            Statement::with_params(
                "INSERT INTO entity_merge
                 (merge_id, scope_id, survivor, absorbed, merged_at, retain_until)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                vec![
                    Value::from(merge_id.as_str()),
                    Value::from(scope_id),
                    Value::from(survivor.as_str()),
                    Value::from(absorbed.as_str()),
                    Value::Integer(seconds(now)),
                    Value::Integer(seconds(UnixSeconds::new(
                        now.get().saturating_add(MERGE_RETENTION_SECONDS),
                    ))),
                ],
            ),
        ])
        .await?;
    Ok(merge_id)
}

/// Reverse a merge inside its retention window (B-2).
///
/// # Errors
///
/// [`Error::Validation`] for an unknown, already reversed, or expired merge;
/// store errors.
pub async fn unmerge(
    store: &StoreHandle,
    scope_id: &str,
    merge_id: &str,
    now: UnixSeconds,
) -> Result<(), Error> {
    let rows: Vec<MergeRow> = store
        .query_consistent(
            "SELECT survivor, absorbed, retain_until, reversed_at FROM entity_merge
             WHERE scope_id = ?1 AND merge_id = ?2",
            vec![Value::from(scope_id), Value::from(merge_id)],
        )
        .await?;
    let row = rows
        .into_iter()
        .next()
        .ok_or_else(|| Error::Validation(format!("merge {merge_id} is not in this scope")))?;
    if row.reversed_at.is_some() {
        return Err(Error::Validation(format!(
            "merge {merge_id} was already reversed"
        )));
    }
    if seconds(now) > row.retain_until {
        return Err(Error::Validation(format!(
            "merge {merge_id} is past its reversal window"
        )));
    }
    store
        .txn(vec![
            Statement::with_params(
                "UPDATE entity SET merged_into = NULL, merged_at = NULL
                 WHERE scope_id = ?1 AND entity_id = ?2 AND merged_into = ?3",
                vec![
                    Value::from(scope_id),
                    Value::from(row.absorbed.as_str()),
                    Value::from(row.survivor.as_str()),
                ],
            ),
            Statement::with_params(
                "UPDATE entity_merge SET reversed_at = ?3
                 WHERE scope_id = ?1 AND merge_id = ?2",
                vec![
                    Value::from(scope_id),
                    Value::from(merge_id),
                    Value::Integer(seconds(now)),
                ],
            ),
        ])
        .await?;
    Ok(())
}
