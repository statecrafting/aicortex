//! Extraction as a job on the embedding outbox (spec 017 B-7).
//!
//! Extraction work lives in the same Rahi processing table and namespace as
//! embedding, under its own processor partition, so it inherits the same
//! claim fencing, backoff, and dead-letter discipline instead of inventing
//! its own. It is idempotent per `(memory_id, extractor_version)`: the
//! processing key carries the version, and a retried commit mints the same
//! deterministic ids.
//!
//! A drain claims one job, reads the memory, runs the extractor, resolves
//! mentions, and commits entities, mentions, edges, contradictions, and the
//! completion of the job in one transaction.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use aicortex_index::{decode, normalize as normalize_vector};
use aicortex_store::{
    EMBEDDING_NAMESPACE, embedding_memory, extraction_processor, scope_keys_after,
    stage_extraction, stage_live_memory_guard,
};
use aicortex_types::{MemoryId, Status};
use rahi_store::{
    Blob, Claim, FailureDetail, RetryPolicy, Statement, StoreHandle, TxnBuilder, Value, Work,
};
use rahi_types::{Error, UnixSeconds};
use serde::Deserialize;

use crate::edge::{EdgeRun, EntityRef, PlannedEdge, plan_edges};
use crate::entity::{
    EntityId, NewEntity, derived_id, normalize, stage_alias, stage_entity, stage_seen,
};
use crate::extract::{Extractor, KnownEntity, ObjectProposal, validate};
use crate::resolve::{ContextVector, MentionRef, Resolution, Via, resolve};
use crate::vocabulary::EntityKind;

/// The most mentions one memory may record.
pub const MAX_MENTIONS: usize = 500;

/// The most known entities offered to an extractor.
pub const MAX_KNOWN: i64 = 5000;

/// The most memories one scheduling pass stages.
pub const MAX_SCHEDULE_BATCH: u32 = 500;

/// Bounds for one drain.
#[derive(Clone, Copy, Debug)]
pub struct ExtractionConfig {
    /// Maximum jobs claimed in one drain.
    pub batch_size: u32,
    /// How long one claim remains valid.
    pub hold_for: Duration,
    /// Exponential retry and dead-letter policy.
    pub retry: RetryPolicy,
}

impl ExtractionConfig {
    /// Sixteen jobs a drain, a minute per claim, five attempts.
    #[must_use]
    pub const fn standard() -> Self {
        Self {
            batch_size: 16,
            hold_for: Duration::from_secs(60),
            retry: RetryPolicy {
                max_attempts: 5,
                base: Duration::from_secs(2),
                cap: Duration::from_secs(300),
            },
        }
    }
}

/// What one drain observed and committed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DrainReport {
    /// Claims reserved.
    pub claimed: u64,
    /// Claims whose graph rows and completion committed together.
    pub completed: u64,
    /// Claims moved to a later retry.
    pub failed: u64,
    /// Claims moved to the dead letter.
    pub dead: u64,
    /// Claims held because the memory is quarantined.
    pub quarantined: u64,
    /// Edges the vocabulary or the floor refused, across the drain.
    pub rejected_edges: u64,
}

/// A worker bound to one extractor.
#[derive(Clone, Debug)]
pub struct ExtractionWorker<E> {
    extractor: E,
    config: ExtractionConfig,
    holder: String,
    processor: String,
}

enum Outcome {
    Completed { rejected: u64 },
    Quarantined,
}

#[derive(Debug, Deserialize)]
struct KnownRow {
    entity_id: String,
    kind: String,
    alias: String,
}

#[derive(Debug, Deserialize)]
struct ContextRow {
    model_id: String,
    model_revision: i64,
    dims: i64,
    normalized: i64,
    vector: Blob,
}

#[derive(Debug, Deserialize)]
struct IdRow {
    id: String,
}

impl<E: Extractor> ExtractionWorker<E> {
    /// Bind a worker.
    ///
    /// # Errors
    ///
    /// [`Error::Config`] for zero bounds or an empty holder.
    pub fn new(
        extractor: E,
        config: ExtractionConfig,
        holder: impl Into<String>,
    ) -> Result<Self, Error> {
        let holder = holder.into();
        if holder.is_empty()
            || config.batch_size == 0
            || config.hold_for < Duration::from_secs(2)
            || config.retry.max_attempts == 0
            || config.retry.base.is_zero()
            || config.retry.cap.is_zero()
        {
            return Err(Error::Config(
                "extraction worker bounds must be non-zero and it needs a holder".to_owned(),
            ));
        }
        let processor = extraction_processor(extractor.version());
        Ok(Self {
            extractor,
            config,
            holder,
            processor,
        })
    }

    /// The extractor this worker runs.
    pub const fn extractor(&self) -> &E {
        &self.extractor
    }

    /// Drain one bounded batch.
    ///
    /// # Errors
    ///
    /// Store errors while claiming or committing work.
    pub async fn drain(&self, store: &StoreHandle, now: UnixSeconds) -> Result<DrainReport, Error> {
        let lease = store
            .lease(&format!(
                "aicortex.graph.worker/{}",
                self.extractor.version()
            ))
            .await?;
        let result = self.drain_leased(store, now).await;
        lease.release().await;
        result
    }

    async fn drain_leased(
        &self,
        store: &StoreHandle,
        now: UnixSeconds,
    ) -> Result<DrainReport, Error> {
        // Expired claims are closed by the namespace sweep, with its own
        // terminal ceiling disabled: the namespace is shared with embedding,
        // whose budget rules differ. The ceiling is enforced per claim below.
        let sweep_policy = RetryPolicy {
            max_attempts: u32::MAX,
            ..self.config.retry
        };
        Work::sweep(
            store,
            EMBEDDING_NAMESPACE,
            &sweep_policy,
            now,
            self.config.batch_size,
        )
        .await?;
        let mut report = DrainReport::default();
        let started = Instant::now();
        for _ in 0..self.config.batch_size {
            let at = elapsed(now, started);
            let mut claims = Work::next(
                store,
                EMBEDDING_NAMESPACE,
                &self.processor,
                &self.holder,
                self.config.hold_for,
                at,
                1,
            )
            .await?;
            let Some(claim) = claims.pop() else { break };
            report.claimed = report.claimed.saturating_add(1);
            if claim.attempt > self.config.retry.max_attempts {
                self.fail(store, &claim, "claim_expired", true, at).await?;
                report.dead = report.dead.saturating_add(1);
                continue;
            }
            match self.process(store, &claim, now, started).await {
                Ok(Outcome::Completed { rejected }) => {
                    report.completed = report.completed.saturating_add(1);
                    report.rejected_edges = report.rejected_edges.saturating_add(rejected);
                }
                Ok(Outcome::Quarantined) => {
                    self.fail(store, &claim, "quarantined", true, elapsed(now, started))
                        .await?;
                    report.quarantined = report.quarantined.saturating_add(1);
                }
                Err(Error::Conflict(_)) => {}
                Err(error) => {
                    let dead = claim.attempt >= self.config.retry.max_attempts;
                    self.fail(store, &claim, error.kind(), false, elapsed(now, started))
                        .await?;
                    if dead {
                        report.dead = report.dead.saturating_add(1);
                    } else {
                        report.failed = report.failed.saturating_add(1);
                    }
                }
            }
        }
        Ok(report)
    }

    async fn fail(
        &self,
        store: &StoreHandle,
        claim: &Claim,
        class: &str,
        terminal: bool,
        now: UnixSeconds,
    ) -> Result<(), Error> {
        let policy = if terminal {
            RetryPolicy {
                max_attempts: claim.attempt,
                ..self.config.retry
            }
        } else {
            self.config.retry
        };
        let mut txn = TxnBuilder::new();
        Work::fail(
            &mut txn,
            claim,
            &FailureDetail {
                class: class.to_owned(),
                detail: None,
            },
            &policy,
            now,
        )?;
        match store.txn(txn.into_statements()).await {
            Ok(_) | Err(Error::Conflict(_)) => Ok(()),
            Err(error) => Err(error),
        }
    }

    async fn complete_empty(
        store: &StoreHandle,
        claim: &Claim,
        now: UnixSeconds,
    ) -> Result<Outcome, Error> {
        let mut txn = TxnBuilder::new();
        Work::complete(&mut txn, claim, now);
        store.txn(txn.into_statements()).await?;
        Ok(Outcome::Completed { rejected: 0 })
    }

    async fn process(
        &self,
        store: &StoreHandle,
        claim: &Claim,
        origin: UnixSeconds,
        started: Instant,
    ) -> Result<Outcome, Error> {
        let now = elapsed(origin, started);
        let scope_id = claim.key.receipt.tenant.as_str();
        let memory_id =
            claim.key.receipt.key.parse::<MemoryId>().map_err(|e| {
                Error::Integrity(format!("extraction work memory id is invalid: {e}"))
            })?;
        let Some(row) = embedding_memory(store, scope_id, memory_id).await? else {
            return Self::complete_empty(store, claim, now).await;
        };
        if row.status == Status::Erased.label() {
            return Self::complete_empty(store, claim, now).await;
        }
        if row.status == Status::Quarantined.label() {
            return Ok(Outcome::Quarantined);
        }
        let memory = row.memory.ok_or_else(|| {
            Error::Integrity(format!("live memory {memory_id} has no decodable record"))
        })?;
        let known = known_entities(store, scope_id).await?;
        let validated = validate(self.extractor.extract(&memory.body.text, &known));
        let rejected = u64::try_from(validated.rejected.len()).unwrap_or(u64::MAX);
        let seen = memory.created;

        let mut txn = TxnBuilder::new();
        stage_live_memory_guard(&mut txn, scope_id, memory_id);
        let mut cache: BTreeMap<(EntityKind, String), EntityId> = BTreeMap::new();
        txn.push(Statement::with_params(
            "DELETE FROM mention WHERE scope_id = ?1 AND memory_id = ?2",
            vec![Value::from(scope_id), Value::from(memory_id.to_string())],
        ));
        for mention in validated.mentions.iter().take(MAX_MENTIONS) {
            let key = (mention.kind, normalize(&mention.surface));
            let entity = if let Some(found) = cache.get(&key) {
                found.clone()
            } else {
                let context =
                    context_vector(store, scope_id, memory_id, mention.start, mention.end).await?;
                let resolution = resolve(
                    store,
                    scope_id,
                    &MentionRef {
                        surface: &mention.surface,
                        kind: mention.kind,
                        context: context.as_ref(),
                    },
                )
                .await?;
                let id = match resolution {
                    Resolution::Existing { entity, via } => {
                        if via != Via::Alias {
                            stage_alias(&mut txn, scope_id, &entity, &mention.surface);
                        }
                        entity
                    }
                    Resolution::New => {
                        let id = EntityId::new(derived_id(
                            "ent",
                            &[
                                scope_id,
                                mention.kind.label(),
                                &key.1,
                                &memory_id.to_string(),
                                &mention.start.to_string(),
                            ],
                        ));
                        stage_entity(
                            &mut txn,
                            &NewEntity {
                                id: &id,
                                scope_id,
                                kind: mention.kind,
                                canonical: &mention.surface,
                                seen,
                                vector: context.as_ref(),
                            },
                        );
                        id
                    }
                };
                cache.insert(key, id.clone());
                id
            };
            stage_seen(&mut txn, scope_id, &entity, seen);
            txn.push(Statement::with_params(
                "INSERT INTO mention
                 (scope_id, memory_id, entity_id, byte_start, byte_end, confidence,
                  extractor, extractor_version)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT DO NOTHING",
                vec![
                    Value::from(scope_id),
                    Value::from(memory_id.to_string()),
                    Value::from(entity.as_str()),
                    Value::Integer(i64::try_from(mention.start).unwrap_or(i64::MAX)),
                    Value::Integer(i64::try_from(mention.end).unwrap_or(i64::MAX)),
                    Value::Integer(i64::from(mention.confidence)),
                    Value::from(self.extractor.name()),
                    Value::Integer(i64::from(self.extractor.version())),
                ],
            ));
        }
        let lookup =
            |surface: &str, kind: EntityKind| cache.get(&(kind, normalize(surface))).cloned();
        let planned: Vec<PlannedEdge> = validated
            .edges
            .iter()
            .filter_map(|edge| {
                let subject = lookup(&edge.subject, edge.subject_kind)?;
                let object = match &edge.object {
                    ObjectProposal::Entity { surface, kind } => {
                        EntityRef::Entity(lookup(surface, *kind)?)
                    }
                    ObjectProposal::Literal(text) => EntityRef::Literal(text.clone()),
                };
                Some(PlannedEdge {
                    subject,
                    predicate: edge.predicate,
                    object,
                    confidence: edge.confidence,
                })
            })
            .collect();
        let run = EdgeRun {
            scope_id,
            memory: memory_id,
            extractor: self.extractor.name(),
            version: self.extractor.version(),
            valid_from: seen,
            now,
        };
        for statement in plan_edges(store, &run, &planned).await? {
            txn.push(statement);
        }
        txn.push(Statement::with_params(
            "INSERT INTO extraction (scope_id, memory_id, extractor, extractor_version, rejected, at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT (scope_id, memory_id, extractor_version)
             DO UPDATE SET rejected = excluded.rejected, at = excluded.at",
            vec![
                Value::from(scope_id),
                Value::from(memory_id.to_string()),
                Value::from(self.extractor.name()),
                Value::Integer(i64::from(self.extractor.version())),
                Value::Integer(i64::try_from(rejected).unwrap_or(i64::MAX)),
                Value::Integer(i64::try_from(now.get()).unwrap_or(i64::MAX)),
            ],
        ));
        Work::complete(&mut txn, claim, now);
        store.txn(txn.into_statements()).await?;
        Ok(Outcome::Completed { rejected })
    }
}

fn elapsed(origin: UnixSeconds, started: Instant) -> UnixSeconds {
    UnixSeconds::new(origin.get().saturating_add(started.elapsed().as_secs()))
}

async fn known_entities(store: &StoreHandle, scope_id: &str) -> Result<Vec<KnownEntity>, Error> {
    let rows: Vec<KnownRow> = store
        .query_consistent(
            "SELECT COALESCE(e.merged_into, e.entity_id) AS entity_id, e.kind AS kind,
                    a.alias AS alias
             FROM entity_alias a JOIN entity e
               ON e.scope_id = a.scope_id AND e.entity_id = a.entity_id
             WHERE a.scope_id = ?1
             ORDER BY e.last_seen DESC, e.entity_id, a.alias LIMIT ?2",
            vec![Value::from(scope_id), Value::Integer(MAX_KNOWN)],
        )
        .await?;
    let mut by_entity: BTreeMap<String, KnownEntity> = BTreeMap::new();
    for row in rows {
        let Some(kind) = EntityKind::parse(&row.kind) else {
            continue;
        };
        by_entity
            .entry(row.entity_id)
            .or_insert_with(|| KnownEntity {
                kind,
                surfaces: Vec::new(),
            })
            .surfaces
            .push(row.alias);
    }
    Ok(by_entity.into_values().collect())
}

/// The embedding of the chunk covering a mention, at the active model
/// revision, with that model's identity (B-2). `None` while the memory has no
/// embedding yet.
async fn context_vector(
    store: &StoreHandle,
    scope_id: &str,
    memory_id: MemoryId,
    start: usize,
    end: usize,
) -> Result<Option<ContextVector>, Error> {
    let rows: Vec<ContextRow> = store
        .query_consistent(
            "SELECT e.model_id AS model_id, e.model_revision AS model_revision, e.dims AS dims,
                    e.normalized AS normalized, e.vector AS vector
             FROM chunk c JOIN embedding e
               ON e.scope_id = c.scope_id AND e.memory_id = c.memory_id
              AND e.model_revision = c.model_revision AND e.chunk_ordinal = c.ordinal
             WHERE c.scope_id = ?1 AND c.memory_id = ?2
               AND c.byte_start < ?4 AND c.byte_end > ?3
               AND c.model_revision = (SELECT revision FROM embedding_model WHERE active = 1)
             ORDER BY c.ordinal LIMIT 1",
            vec![
                Value::from(scope_id),
                Value::from(memory_id.to_string()),
                Value::Integer(i64::try_from(start).unwrap_or(i64::MAX)),
                Value::Integer(i64::try_from(end).unwrap_or(i64::MAX)),
            ],
        )
        .await?;
    let Some(row) = rows.into_iter().next() else {
        return Ok(None);
    };
    let dims = u16::try_from(row.dims)
        .map_err(|_| Error::Integrity("stored vector width is outside u16".to_owned()))?;
    let mut values = decode(row.vector.as_slice(), dims)
        .map_err(|e| Error::Integrity(format!("stored chunk vector: {e}")))?;
    if row.normalized == 0 && normalize_vector(&mut values).is_err() {
        return Ok(None);
    }
    Ok(Some(ContextVector {
        model_id: row.model_id,
        revision: u32::try_from(row.model_revision)
            .map_err(|_| Error::Integrity("stored model revision is outside u32".to_owned()))?,
        dims,
        normalized: row.normalized != 0,
        values,
    }))
}

/// Progress from one bounded scheduling pass.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScheduleBatch {
    /// Jobs newly staged.
    pub staged: u32,
    /// The last memory considered, for the next pass.
    pub cursor: Option<MemoryId>,
    /// Whether further eligible memories were observed.
    pub more: bool,
}

/// Stage extraction work for live memories of `scope_id` that have no
/// extraction at this extractor's version (B-7).
///
/// This is how a new extractor version reaches memories already extracted
/// and how memories captured while no model was active get their first
/// extraction. Staging is idempotent under Rahi's processing key.
///
/// # Errors
///
/// [`Error::Config`] for a limit outside `1..=MAX_SCHEDULE_BATCH`; store
/// errors.
pub async fn schedule(
    store: &StoreHandle,
    scope_id: &str,
    extractor: &dyn Extractor,
    cursor: Option<MemoryId>,
    limit: u32,
    now: UnixSeconds,
) -> Result<ScheduleBatch, Error> {
    if limit == 0 || limit > MAX_SCHEDULE_BATCH {
        return Err(Error::Config(format!(
            "extraction scheduling limit must be in 1..={MAX_SCHEDULE_BATCH}"
        )));
    }
    let rows: Vec<IdRow> = store
        .query_consistent(
            "SELECT m.id AS id FROM memory m
             WHERE m.scope_id = ?1 AND m.status NOT IN ('erased', 'quarantined') AND m.id > ?2
               AND NOT EXISTS (
                 SELECT 1 FROM extraction x
                 WHERE x.scope_id = ?1 AND x.memory_id = m.id AND x.extractor_version = ?3)
             ORDER BY m.id LIMIT ?4",
            vec![
                Value::from(scope_id),
                Value::from(cursor.map_or_else(String::new, |id| id.to_string())),
                Value::Integer(i64::from(extractor.version())),
                Value::Integer(i64::from(limit) + 1),
            ],
        )
        .await?;
    let take = usize::try_from(limit).unwrap_or(usize::MAX);
    let more = rows.len() > take;
    let mut txn = TxnBuilder::new();
    let mut last = None;
    for row in rows.into_iter().take(take) {
        let id = row
            .id
            .parse::<MemoryId>()
            .map_err(|e| Error::Integrity(format!("stored memory id is invalid: {e}")))?;
        stage_live_memory_guard(&mut txn, scope_id, id);
        stage_extraction(
            &mut txn,
            scope_id,
            id,
            extractor.name(),
            extractor.version(),
            now,
        )?;
        last = Some(id);
    }
    let staged = if txn.is_empty() {
        0
    } else {
        let results = store.txn(txn.into_statements()).await?;
        // Every memory stages a guard and a work row; the work row reports
        // one affected row only when it was new.
        let new = results
            .iter()
            .skip(1)
            .step_by(2)
            .filter(|r| r.rows_affected == 1)
            .count();
        u32::try_from(new).unwrap_or(u32::MAX)
    };
    Ok(ScheduleBatch {
        staged,
        cursor: last.or(cursor),
        more,
    })
}

/// One scheduling pass over every scope, bounded by `per_scope`.
///
/// # Errors
///
/// Store errors.
pub async fn schedule_deployment(
    store: &StoreHandle,
    extractor: &dyn Extractor,
    per_scope: u32,
    now: UnixSeconds,
) -> Result<u64, Error> {
    let mut staged = 0_u64;
    let mut after: Option<String> = None;
    loop {
        let page = scope_keys_after(store, after.as_deref(), 100).await?;
        let Some(last) = page.last().cloned() else {
            return Ok(staged);
        };
        for scope_id in &page {
            let batch = schedule(store, scope_id, extractor, None, per_scope, now).await?;
            staged = staged.saturating_add(u64::from(batch.staged));
        }
        after = Some(last);
    }
}
