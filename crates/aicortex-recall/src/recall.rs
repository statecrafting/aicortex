//! The recall pipeline (spec 018 B-1 to B-11).
//!
//! One [`Recall::recall`] call embeds the query, runs the enabled channels
//! concurrently under one budget, fuses them, reads the importance prior of
//! what was proposed, and returns ids with their explanation. It returns no
//! memory body: a surface loads the memories the ids name and frames them
//! through spec 019's envelope, so this crate's output never has to be.

use std::collections::BTreeMap;
use std::time::Instant;

use aicortex_embed::{EmbeddingProvider, ModelRegistry};
use aicortex_index::Comparable;
use aicortex_store::ScopeId;
use aicortex_types::{Importance, MemoryId};
use rahi_store::{StoreHandle, Value};
use rahi_types::{Error, UnixSeconds};
use serde::{Deserialize, Serialize};

use crate::channels::{self, ChannelOutput, Env, json_ids};
use crate::freshness::{self, Freshness};
use crate::fusion::{fuse, prior};
use crate::query::{Channel, Clock, Query, RecallConfig};
use crate::trace::{ByteRange, ChannelTrace, RecallTrace, TraceCandidate};

/// One returned memory: its id, its place, and the sentence-level pointer.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RecalledMemory {
    /// The memory. Its body is loaded and framed by the surface (019).
    pub memory_id: MemoryId,
    /// Its place in the result, from 1.
    pub rank: u32,
    /// The fused score.
    pub score: f64,
    /// The winning chunk's byte range (B-4).
    pub chunk: Option<ByteRange>,
}

/// A result set and its explanation.
#[derive(Clone, Debug, PartialEq)]
pub struct RecallResult {
    /// The memories, best first.
    pub memories: Vec<RecalledMemory>,
    /// Why they are in this order.
    pub trace: RecallTrace,
}

impl RecallResult {
    /// The freshness block of B-11.
    #[must_use]
    pub const fn freshness(&self) -> &Freshness {
        &self.trace.freshness
    }

    /// Whether any channel was cut short by the budget.
    #[must_use]
    pub const fn truncated(&self) -> bool {
        self.trace.truncated
    }
}

/// Recall over one store with one embedder and one clock.
#[derive(Debug)]
pub struct Recall<P, C> {
    store: StoreHandle,
    provider: P,
    clock: C,
    config: RecallConfig,
}

#[derive(Debug, Deserialize)]
struct ImportanceRow {
    id: String,
    base: Option<f64>,
    last_used: Option<i64>,
    uses: Option<i64>,
}

impl<P: EmbeddingProvider, C: Clock> Recall<P, C> {
    /// Recall over `store`, embedding queries with `provider`.
    #[must_use]
    pub const fn new(store: StoreHandle, provider: P, clock: C, config: RecallConfig) -> Self {
        Self {
            store,
            provider,
            clock,
            config,
        }
    }

    /// Run `query` (B-1 to B-11).
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] for a filter outside its vocabulary; store
    /// errors. A channel that cannot run (no active model, a provider that
    /// is not the active model, an embedding failure) is skipped and says so
    /// in the trace rather than failing the query.
    pub async fn recall(&self, query: &Query) -> Result<RecallResult, Error> {
        query.filter.validate()?;
        let started = Instant::now();
        let now = self.clock.now();
        let scope = ScopeId::of(&query.scope);
        let limit = query.k.saturating_mul(self.config.overfetch.max(1));
        let env = Env {
            store: &self.store,
            scope: &scope,
            filter: &query.filter,
            text: &query.text,
            limit,
            budget: query.budget,
            started,
            now,
            config: &self.config,
        };

        let active = ModelRegistry::active(&self.store).await?;
        let needle = self.embed(query, active.as_ref()).await;
        let entities = channels::resolve_entities(&self.store, &scope, &query.text).await?;

        let enabled = |channel| query.k > 0 && query.channels.contains(channel);
        let (vector, text, recency, graph) = tokio::join!(
            async {
                if !enabled(Channel::Vector) {
                    return Ok(ChannelOutput::skipped(Channel::Vector, "disabled"));
                }
                match (&active, needle) {
                    (Some(model), Ok(needle)) => {
                        let comparable = Comparable {
                            model_id: model.model_id.as_str(),
                            revision: model.revision,
                            dims: model.dims,
                            normalized: model.normalized,
                        };
                        channels::vector(&env, comparable, &needle).await
                    }
                    (None, _) => Ok(ChannelOutput::skipped(
                        Channel::Vector,
                        "no active model revision",
                    )),
                    (_, Err(reason)) => Ok(ChannelOutput::skipped(Channel::Vector, reason)),
                }
            },
            async {
                if !enabled(Channel::Text) {
                    return Ok(ChannelOutput::skipped(Channel::Text, "disabled"));
                }
                match &active {
                    Some(model) => channels::text(&env, model.revision).await,
                    None => Ok(ChannelOutput::skipped(
                        Channel::Text,
                        "no active model revision",
                    )),
                }
            },
            async {
                if enabled(Channel::Recency) {
                    channels::recency(&env).await
                } else {
                    Ok(ChannelOutput::skipped(Channel::Recency, "disabled"))
                }
            },
            async {
                if enabled(Channel::Graph) {
                    channels::graph(&env, &entities).await
                } else {
                    Ok(ChannelOutput::skipped(Channel::Graph, "disabled"))
                }
            },
        );
        let mut outputs: Vec<ChannelOutput> = vec![vector?, text?, recency?, graph?];

        // B-4: the winning chunk's range travels with the result.
        let chunk_ids: Vec<String> = outputs
            .iter()
            .flat_map(|output| output.candidates.iter())
            .filter_map(|candidate| candidate.chunk_id.clone())
            .collect();
        let ranges = channels::chunk_ranges(&self.store, &scope, &chunk_ids).await?;
        for candidate in outputs.iter_mut().flat_map(|o| o.candidates.iter_mut()) {
            candidate.range = candidate
                .chunk_id
                .as_ref()
                .and_then(|id| ranges.get(id).copied());
        }

        let proposed: Vec<MemoryId> = outputs
            .iter()
            .flat_map(|output| output.candidates.iter().map(|c| c.memory_id))
            .collect();
        let priors = self.priors(&scope, &proposed, now).await?;
        let fused = fuse(&outputs, &priors, &self.config, query.k);

        let returned: Vec<MemoryId> = fused.iter().map(|entry| entry.memory_id).collect();
        let freshness = freshness::compute(
            &self.store,
            &scope,
            &entities,
            &returned,
            &query.filter.sources,
            now,
            self.config.freshness_limit,
        )
        .await?;

        let memories = fused
            .iter()
            .map(|entry| RecalledMemory {
                memory_id: entry.memory_id,
                rank: entry.rank,
                score: entry.score,
                chunk: entry.chunk,
            })
            .collect();
        let truncated = outputs.iter().any(|output| output.truncated);
        let per_channel = outputs
            .iter()
            .map(|output| ChannelTrace {
                channel: output.channel,
                count: output.candidates.len(),
                truncated: output.truncated,
                skipped: output.skipped.clone(),
                candidates: if query.explain {
                    output
                        .candidates
                        .iter()
                        .enumerate()
                        .map(|(position, c)| TraceCandidate {
                            memory_id: c.memory_id,
                            rank: u32::try_from(position + 1).unwrap_or(u32::MAX),
                            score: c.score,
                            chunk: c.range,
                            hops: c.hops,
                        })
                        .collect()
                } else {
                    Vec::new()
                },
            })
            .collect();
        // D-2: the trace is always computed; `explain` only trims it.
        let fused = fused
            .into_iter()
            .map(|mut entry| {
                if !query.explain {
                    entry.contributions.clear();
                }
                entry
            })
            .collect();
        let trace = RecallTrace {
            query_hash: query.hash(scope.as_str()),
            filters: query.filter.clone(),
            per_channel,
            fused,
            weights: self.config.weights,
            rank_constant: self.config.rank_constant,
            priors: if query.explain {
                priors
            } else {
                BTreeMap::new()
            },
            budget_used_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            truncated,
            freshness,
        };
        Ok(RecallResult { memories, trace })
    }

    /// The query vector, or the reason there is none.
    async fn embed(
        &self,
        query: &Query,
        active: Option<&aicortex_embed::ModelRevision>,
    ) -> Result<Vec<f32>, String> {
        if !query.channels.contains(Channel::Vector) || query.k == 0 {
            return Err("disabled".to_owned());
        }
        let model = active.ok_or_else(|| "no active model revision".to_owned())?;
        // A query only compares against its own model's vectors.
        if model.model_id != self.provider.id() || model.dims != self.provider.dims() {
            return Err("the provider is not the active model".to_owned());
        }
        let mut vectors = self
            .provider
            .embed(&[query.text.as_str()])
            .await
            .map_err(|error| format!("embedding failed: {error}"))?;
        vectors
            .pop()
            .map(|vector| vector.values().to_vec())
            .ok_or_else(|| "the provider returned no vector".to_owned())
    }

    /// The importance prior of each of `ids` at `now` (B-3).
    async fn priors(
        &self,
        scope: &ScopeId,
        ids: &[MemoryId],
        now: UnixSeconds,
    ) -> Result<BTreeMap<MemoryId, f64>, Error> {
        if ids.is_empty() {
            return Ok(BTreeMap::new());
        }
        let ids: Vec<String> = ids.iter().map(ToString::to_string).collect();
        let rows: Vec<ImportanceRow> = self
            .store
            .query(
                "SELECT id,
                        json_extract(record, '$.importance.base') AS base,
                        json_extract(record, '$.importance.last_used') AS last_used,
                        json_extract(record, '$.importance.uses') AS uses
                 FROM memory WHERE scope_id = ?1
                   AND id IN (SELECT value FROM json_each(?2))"
                    .to_owned(),
                vec![Value::from(scope.as_str()), Value::from(json_ids(&ids))],
            )
            .await?;
        let mut priors = BTreeMap::new();
        for row in rows {
            let id = MemoryId::parse(&row.id)
                .map_err(|error| Error::Integrity(format!("stored memory id: {error}")))?;
            // A record without an importance takes the neutral prior.
            let decayed = match (row.base, row.last_used) {
                (Some(base), Some(last_used)) => Importance::new(
                    base as f32,
                    UnixSeconds::new(u64::try_from(last_used).unwrap_or(0)),
                    u32::try_from(row.uses.unwrap_or(1)).unwrap_or(1),
                )
                .map_or(0.5, |importance| f64::from(importance.decayed_at(now))),
                _ => 0.5,
            };
            priors.insert(id, prior(decayed, self.config.importance_span));
        }
        Ok(priors)
    }
}
