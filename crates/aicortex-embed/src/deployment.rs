//! Deployment-wide embedding health for the operator's verbs (spec 015 B-3).
//!
//! Queue health and coverage are scope-bound statements (D-17, D-18): each
//! one carries a scope predicate and the storage crate offers no method that
//! reads memories across scopes. The operator-wide figure `preflight` and the
//! `/metrics` collector need is therefore a *sum of scoped reads* over the
//! scope inventory, taken inside a time budget. Nothing here widens a tenant
//! surface, and only counts and ages leave: no scope key, memory id, or
//! content appears in the result (D-28).

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use aicortex_store::{ReadStore, scope_keys_after};
use rahi_types::{Error, UnixSeconds};

use crate::registry::{Coverage, ModelRegistry};
use crate::worker::{EmbeddingPreflight, QueueHealth, queue_health};

/// Scope keys read per inventory page.
const SCOPE_PAGE: u32 = 128;

/// The deployment-wide report and how much of the deployment it covers.
#[derive(Clone, Debug)]
pub struct DeploymentHealth {
    /// The summed observations. `active` is the registry's, not a sum.
    pub report: EmbeddingPreflight,
    /// Scopes whose figures are included.
    pub scopes_read: u64,
    /// Whether every scope was read. False means the time budget ended the
    /// walk, so the figures are a lower bound.
    pub complete: bool,
}

impl EmbeddingPreflight {
    /// Sum the scoped observations over every scope, within `budget`.
    ///
    /// The queue's oldest pending age is the maximum over scopes; every other
    /// figure is a sum. The walk stops between scopes once `budget` has
    /// elapsed and reports `complete = false`.
    ///
    /// # Errors
    ///
    /// Store errors or corrupt persisted values.
    pub async fn read_deployment(
        store: &impl ReadStore,
        now: UnixSeconds,
        budget: Duration,
    ) -> Result<DeploymentHealth, Error> {
        let started = Instant::now();
        let active = ModelRegistry::active(store).await?;
        let mut queue = QueueHealth::default();
        let mut coverage: BTreeMap<u32, (u64, u64)> = BTreeMap::new();
        let mut live_memories = 0_u64;
        let mut scopes_read = 0_u64;
        let mut after: Option<String> = None;
        let mut complete = true;
        'pages: loop {
            let page = scope_keys_after(store, after.as_deref(), SCOPE_PAGE).await?;
            let Some(last) = page.last().cloned() else {
                break;
            };
            for scope_id in &page {
                if started.elapsed() >= budget {
                    complete = false;
                    break 'pages;
                }
                let health = queue_health(store, scope_id, now).await?;
                queue.pending = queue.pending.saturating_add(health.pending);
                queue.dead = queue.dead.saturating_add(health.dead);
                queue.quarantined = queue.quarantined.saturating_add(health.quarantined);
                queue.oldest_pending_age_seconds = match (
                    queue.oldest_pending_age_seconds,
                    health.oldest_pending_age_seconds,
                ) {
                    (Some(left), Some(right)) => Some(left.max(right)),
                    (left, right) => left.or(right),
                };
                live_memories =
                    live_memories.saturating_add(ModelRegistry::live_total(store, scope_id).await?);
                for item in ModelRegistry::coverage(store, scope_id).await? {
                    let entry = coverage.entry(item.revision).or_default();
                    entry.0 = entry.0.saturating_add(item.embedded);
                    entry.1 = entry.1.saturating_add(item.total);
                }
                scopes_read = scopes_read.saturating_add(1);
            }
            after = Some(last);
        }
        Ok(DeploymentHealth {
            report: EmbeddingPreflight {
                active,
                queue,
                coverage: coverage
                    .into_iter()
                    .map(|(revision, (embedded, total))| Coverage {
                        revision,
                        embedded,
                        total,
                    })
                    .collect(),
                live_memories,
            },
            scopes_read,
            complete,
        })
    }
}
