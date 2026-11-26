//! The four candidate channels (spec 018 B-2, B-7, B-9).
//!
//! Every channel states the scope, the admitted statuses, and the filter's
//! predicates in its own statement. The vector and text channels delegate to
//! spec 016's scan and search, which render the filter themselves; the
//! recency and graph channels render it here, from the same
//! [`aicortex_index::Restriction`].

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use aicortex_graph::{EntityId, WalkOptions, neighbors, normalize};
use aicortex_index::text::{TextQuery, search};
use aicortex_index::tokenize::TokenizerConfig;
use aicortex_index::{Comparable, ScanConfig, ScanOutcome, ScanQuery, scan};
use aicortex_store::ScopeId;
use aicortex_types::MemoryId;
use rahi_store::{StoreHandle, Value};
use rahi_types::{Error, UnixSeconds};
use serde::Deserialize;

use crate::filter::Filter;
use crate::query::{Channel, RECENCY_WINDOW_HALF_LIVES, RecallConfig};
use crate::trace::ByteRange;

/// The most entities one query resolves.
const MAX_ENTITIES: i64 = 16;

/// One memory a channel proposes.
#[derive(Clone, Debug, PartialEq)]
pub struct Ranked {
    /// The memory.
    pub memory_id: MemoryId,
    /// The channel's score.
    pub score: f64,
    /// The best chunk, for the vector and text channels.
    pub chunk_id: Option<String>,
    /// The best chunk's byte range, filled in after the channels return.
    pub range: Option<ByteRange>,
    /// Path length from a query entity, for the graph channel.
    pub hops: Option<u8>,
}

/// What a channel returned: candidates best first, one per memory.
#[derive(Clone, Debug, PartialEq)]
pub struct ChannelOutput {
    /// The channel.
    pub channel: Channel,
    /// Best first.
    pub candidates: Vec<Ranked>,
    /// Whether the budget cut it short.
    pub truncated: bool,
    /// Why it did not run.
    pub skipped: Option<String>,
}

impl ChannelOutput {
    /// A channel that returned nothing for `reason`.
    #[must_use]
    pub fn skipped(channel: Channel, reason: impl Into<String>) -> Self {
        Self {
            channel,
            candidates: Vec::new(),
            truncated: false,
            skipped: Some(reason.into()),
        }
    }

    fn empty(channel: Channel) -> Self {
        Self {
            channel,
            candidates: Vec::new(),
            truncated: false,
            skipped: None,
        }
    }
}

/// What every channel needs.
pub struct Env<'a> {
    /// The store.
    pub store: &'a StoreHandle,
    /// The scope id every statement carries.
    pub scope: &'a ScopeId,
    /// The query's filter.
    pub filter: &'a Filter,
    /// The query text.
    pub text: &'a str,
    /// Candidates wanted per channel (`k * overfetch`).
    pub limit: usize,
    /// The shared budget.
    pub budget: Duration,
    /// When the query started.
    pub started: Instant,
    /// The injected clock's reading.
    pub now: UnixSeconds,
    /// Configuration.
    pub config: &'a RecallConfig,
}

impl Env<'_> {
    fn scan_config(&self) -> ScanConfig {
        ScanConfig {
            budget: self.budget.saturating_sub(self.started.elapsed()),
            ..ScanConfig::default()
        }
    }
}

/// Keep each memory's best chunk, in the order given (best first).
fn best_per_memory(outcome: ScanOutcome, channel: Channel) -> ChannelOutput {
    let mut seen = BTreeSet::new();
    let candidates = outcome
        .candidates
        .into_iter()
        .filter(|candidate| seen.insert(candidate.memory_id))
        .map(|candidate| Ranked {
            memory_id: candidate.memory_id,
            score: candidate.score,
            chunk_id: Some(candidate.chunk_id),
            range: None,
            hops: None,
        })
        .collect();
    ChannelOutput {
        channel,
        candidates,
        truncated: outcome.truncated,
        skipped: None,
    }
}

/// The vector channel: the active revision's nearest chunks (B-2).
///
/// # Errors
///
/// Store errors and validation errors from the scan.
pub async fn vector(
    env: &Env<'_>,
    model: Comparable<'_>,
    needle: &[f32],
) -> Result<ChannelOutput, Error> {
    let filter = env.filter.index_filter()?;
    let query = ScanQuery {
        scope: env.scope,
        model,
        vector: needle,
        k: env.limit,
        filter: &filter,
    };
    let outcome = scan(env.store, &query, env.scan_config()).await?;
    Ok(best_per_memory(outcome, Channel::Vector))
}

/// The text channel: BM25 over the active revision's chunks (B-2).
///
/// # Errors
///
/// Store errors.
pub async fn text(env: &Env<'_>, revision: u32) -> Result<ChannelOutput, Error> {
    let filter = env.filter.index_filter()?;
    let query = TextQuery {
        scope: env.scope,
        revision,
        text: env.text,
        k: env.limit,
        filter: &filter,
        tokenizer: TokenizerConfig::default(),
    };
    let outcome = search(env.store, &query, env.scan_config()).await?;
    Ok(best_per_memory(outcome, Channel::Text))
}

/// The admitted-memory predicate over alias `m`: scope `?1`, the status
/// list, and the filter's restriction. Returns the clause, its parameters
/// after the scope, and the next free parameter number.
fn admit(filter: &Filter) -> (String, Vec<Value>, usize) {
    let labels = filter.status_labels();
    let placeholders = (0..labels.len())
        .map(|offset| format!("?{}", 2 + offset))
        .collect::<Vec<_>>()
        .join(", ");
    let (restriction, extra) = filter.restriction().clauses(2 + labels.len());
    let next = 2 + labels.len() + extra.len();
    let mut params: Vec<Value> = labels.iter().map(|label| Value::from(*label)).collect();
    params.extend(extra);
    (
        format!("m.scope_id = ?1 AND m.status IN ({placeholders}){restriction}"),
        params,
        next,
    )
}

#[derive(Debug, Deserialize)]
struct CreatedRow {
    id: String,
    created: i64,
}

#[derive(Debug, Deserialize)]
struct IdRow {
    id: String,
}

fn memory_id(text: &str) -> Result<MemoryId, Error> {
    MemoryId::parse(text).map_err(|error| Error::Integrity(format!("stored memory id: {error}")))
}

/// The recency channel: the newest admitted memories inside the decay
/// window, scored `0.5^(age / half_life)` (B-2). The window and the channel's
/// low weight are what keep it from dominating.
///
/// # Errors
///
/// Store errors.
#[expect(
    clippy::float_arithmetic,
    reason = "a decay over time is floating point; the allow is one function wide"
)]
pub async fn recency(env: &Env<'_>) -> Result<ChannelOutput, Error> {
    env.filter.validate()?;
    let (clause, mut params, next) = admit(env.filter);
    let half_life = env.config.recency_half_life_secs.max(1);
    let window = half_life.saturating_mul(RECENCY_WINDOW_HALF_LIVES);
    let now = env.now.get();
    let oldest = now.saturating_sub(window);
    let sql = format!(
        "SELECT m.id AS id, m.created AS created FROM memory m
         WHERE {clause} AND m.created <= ?{next} AND m.created >= ?{}
         ORDER BY m.created DESC, m.id LIMIT ?{}",
        next + 1,
        next + 2
    );
    let mut all = vec![Value::from(env.scope.as_str())];
    all.append(&mut params);
    all.push(Value::Integer(i64::try_from(now).unwrap_or(i64::MAX)));
    all.push(Value::Integer(i64::try_from(oldest).unwrap_or(0)));
    all.push(Value::Integer(i64::try_from(env.limit).unwrap_or(i64::MAX)));
    let rows: Vec<CreatedRow> = env.store.query(sql, all).await?;
    let mut candidates = Vec::with_capacity(rows.len());
    for row in rows {
        let created = u64::try_from(row.created).unwrap_or(0);
        let age = now.saturating_sub(created) as f64;
        candidates.push(Ranked {
            memory_id: memory_id(&row.id)?,
            score: 0.5_f64.powf(age / half_life as f64),
            chunk_id: None,
            range: None,
            hops: None,
        });
    }
    Ok(ChannelOutput {
        candidates,
        ..ChannelOutput::empty(Channel::Recency)
    })
}

#[derive(Debug, Deserialize)]
struct EntityRow {
    entity_id: String,
}

/// The entities whose alias appears in the query text, merged entities
/// followed to their survivor, in id order (B-2, B-11).
///
/// # Errors
///
/// Store errors.
pub async fn resolve_entities(
    store: &StoreHandle,
    scope: &ScopeId,
    text: &str,
) -> Result<Vec<EntityId>, Error> {
    let folded = normalize(text);
    if folded.is_empty() {
        return Ok(Vec::new());
    }
    let rows: Vec<EntityRow> = store
        .query(
            "SELECT DISTINCT COALESCE(e.merged_into, e.entity_id) AS entity_id
             FROM entity_alias a
             JOIN entity e ON e.scope_id = a.scope_id AND e.entity_id = a.entity_id
             WHERE a.scope_id = ?1 AND a.normalized <> ''
               AND instr(?2, ' ' || a.normalized || ' ') > 0
             ORDER BY 1 LIMIT ?3"
                .to_owned(),
            vec![
                Value::from(scope.as_str()),
                Value::from(format!(" {folded} ")),
                Value::Integer(MAX_ENTITIES),
            ],
        )
        .await?;
    Ok(rows
        .into_iter()
        .map(|row| EntityId::new(row.entity_id))
        .collect())
}

/// The graph channel: memories that mention a resolved entity (path length
/// zero) or that an edge reachable from it was derived from (017 B-9),
/// scored `1 / (1 + hops)` (B-2).
///
/// # Errors
///
/// Store errors.
#[expect(
    clippy::float_arithmetic,
    reason = "the inverse path length is floating point; the allow is one function wide"
)]
pub async fn graph(env: &Env<'_>, entities: &[EntityId]) -> Result<ChannelOutput, Error> {
    env.filter.validate()?;
    let scope = env.scope.as_str();
    let mut reached: BTreeMap<MemoryId, u8> = BTreeMap::new();
    let mut truncated = false;
    for entity in entities {
        if env.started.elapsed() >= env.budget && !reached.is_empty() {
            truncated = true;
            break;
        }
        let mentioned: Vec<IdRow> = env
            .store
            .query(
                "SELECT DISTINCT n.memory_id AS id FROM mention n
                 WHERE n.scope_id = ?1 AND n.entity_id IN
                   (SELECT entity_id FROM entity
                    WHERE scope_id = ?1 AND (entity_id = ?2 OR merged_into = ?2))"
                    .to_owned(),
                vec![Value::from(scope), Value::from(entity.as_str())],
            )
            .await?;
        for row in mentioned {
            reached.insert(memory_id(&row.id)?, 0);
        }
        for hit in neighbors(env.store, scope, entity, &WalkOptions::default()).await? {
            let hops = u8::try_from(hit.path.0.len()).unwrap_or(u8::MAX);
            let slot = reached.entry(hit.memory).or_insert(hops);
            *slot = (*slot).min(hops);
        }
    }
    if reached.is_empty() {
        return Ok(ChannelOutput {
            truncated,
            ..ChannelOutput::empty(Channel::Graph)
        });
    }
    // The walk and the mention read know nothing of the filter: the
    // admitted-memory predicate runs once over everything they reached.
    let (clause, params, next) = admit(env.filter);
    let ids: Vec<String> = reached.keys().map(ToString::to_string).collect();
    let sql = format!(
        "SELECT m.id AS id FROM memory m
         WHERE {clause} AND m.id IN (SELECT value FROM json_each(?{next}))"
    );
    let mut all = vec![Value::from(scope)];
    all.extend(params);
    all.push(Value::from(json_ids(&ids)));
    let admitted: Vec<IdRow> = env.store.query(sql, all).await?;
    let mut candidates = Vec::new();
    for row in admitted {
        let id = memory_id(&row.id)?;
        let hops = reached.get(&id).copied().unwrap_or(u8::MAX);
        candidates.push(Ranked {
            memory_id: id,
            score: 1.0 / (1.0 + f64::from(hops)),
            chunk_id: None,
            range: None,
            hops: Some(hops),
        });
    }
    candidates.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| a.memory_id.cmp(&b.memory_id))
    });
    candidates.truncate(env.limit);
    Ok(ChannelOutput {
        candidates,
        truncated,
        ..ChannelOutput::empty(Channel::Graph)
    })
}

/// A JSON array of ids, for `json_each`.
pub(crate) fn json_ids(ids: &[String]) -> String {
    serde_json::to_string(ids).unwrap_or_else(|_| "[]".to_owned())
}

#[derive(Debug, Deserialize)]
struct RangeRow {
    chunk_id: String,
    byte_start: i64,
    byte_end: i64,
}

/// The byte ranges of `chunk_ids` in `scope` (B-4).
///
/// # Errors
///
/// Store errors.
pub async fn chunk_ranges(
    store: &StoreHandle,
    scope: &ScopeId,
    chunk_ids: &[String],
) -> Result<BTreeMap<String, ByteRange>, Error> {
    if chunk_ids.is_empty() {
        return Ok(BTreeMap::new());
    }
    let rows: Vec<RangeRow> = store
        .query(
            "SELECT chunk_id, byte_start, byte_end FROM chunk
             WHERE scope_id = ?1 AND chunk_id IN
               (SELECT value FROM json_each(?2))"
                .to_owned(),
            vec![
                Value::from(scope.as_str()),
                Value::from(json_ids(chunk_ids)),
            ],
        )
        .await?;
    Ok(rows
        .into_iter()
        .map(|row| {
            (
                row.chunk_id,
                ByteRange {
                    start: u64::try_from(row.byte_start).unwrap_or(0),
                    end: u64::try_from(row.byte_end).unwrap_or(0),
                },
            )
        })
        .collect())
}
