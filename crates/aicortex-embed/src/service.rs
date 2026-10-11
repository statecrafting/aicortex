//! The embedding worker as a service function (spec 015 B-2).
//!
//! [`run_worker`] is the whole lifecycle of the per-node worker: drain,
//! idle, retry, stop. It owns no task. A caller that manages services mounts
//! it as one, hands it a shutdown signal, and joins the future it returns;
//! nothing here spawns, so nothing can outlive the owner that started it.
//!
//! Stopping is safe at any instant. Work is durable and claims are fenced,
//! so a drain cut off mid-flight leaves its rows claimed until the claim
//! expires, after which the next drain reclaims and completes them exactly
//! once. The shutdown path still lets a drain in progress finish for a
//! bounded grace period, because finishing is cheaper than reclaiming.

use std::future::Future;
use std::pin::pin;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rahi_store::{RetryPolicy, StoreHandle};
use rahi_types::{Error, UnixSeconds};

use crate::chunk::{ChunkConfig, Chunker};
use crate::provider::EmbeddingProvider;
use crate::worker::{EmbeddingWorker, WorkerConfig};

/// Pacing for [`run_worker`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ServiceSettings {
    /// How long to wait after a drain that found no work.
    pub idle: Duration,
    /// How long to wait after a drain that failed on the store.
    pub error_backoff: Duration,
    /// How long a drain already in progress may keep running once shutdown
    /// is requested.
    pub shutdown_grace: Duration,
}

impl Default for ServiceSettings {
    fn default() -> Self {
        Self {
            idle: Duration::from_secs(2),
            error_backoff: Duration::from_secs(5),
            shutdown_grace: Duration::from_secs(10),
        }
    }
}

/// What a service run observed, returned when it stops.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ServiceReport {
    /// Drains that completed.
    pub drains: u64,
    /// Claims reserved across all drains.
    pub claimed: u64,
    /// Claims committed with their vectors.
    pub completed: u64,
    /// Claims scheduled for retry.
    pub failed: u64,
    /// Claims moved to the dead letter.
    pub dead: u64,
    /// Claims held as quarantined work.
    pub quarantined: u64,
    /// Drains that failed on the store and were retried after a backoff.
    pub store_errors: u64,
    /// The most recent store error, for the operator.
    pub last_error: Option<String>,
}

/// The default chunking policy: bodies up to 2 KiB stay whole; longer ones
/// split near 1 KiB with 128 bytes of overlap.
#[must_use]
pub const fn default_chunk_config() -> ChunkConfig {
    ChunkConfig {
        threshold_bytes: 2048,
        target_bytes: 1024,
        overlap_bytes: 128,
        max_chunks_per_memory: 512,
    }
}

/// The default worker bounds: sixteen claims per drain, a one-minute hold,
/// five attempts with exponential backoff capped at five minutes.
#[must_use]
pub const fn default_worker_config() -> WorkerConfig {
    WorkerConfig {
        batch_size: 16,
        hold_for: Duration::from_secs(60),
        retry: RetryPolicy {
            max_attempts: 5,
            base: Duration::from_secs(2),
            cap: Duration::from_secs(300),
        },
    }
}

/// The default chunker.
///
/// # Errors
///
/// Never, for the shipped defaults; the signature carries the validation.
pub fn default_chunker() -> Result<Chunker, Error> {
    Chunker::new(default_chunk_config())
}

/// Seconds since the Unix epoch, for claim and backoff arithmetic.
#[must_use]
pub fn unix_now() -> UnixSeconds {
    UnixSeconds::new(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs()),
    )
}

/// Drain the outbox until `shutdown` resolves, then return what happened.
///
/// A store error never ends the run: it is counted, remembered, and retried
/// after [`ServiceSettings::error_backoff`], because the durable queue makes
/// every retry safe.
pub async fn run_worker<P: EmbeddingProvider>(
    worker: &EmbeddingWorker<P>,
    store: &StoreHandle,
    settings: ServiceSettings,
    shutdown: impl Future<Output = ()>,
) -> ServiceReport {
    let mut shutdown = pin!(shutdown);
    let mut report = ServiceReport::default();
    loop {
        let mut drain = pin!(worker.drain(store, unix_now()));
        let outcome = tokio::select! {
            biased;
            () = &mut shutdown => {
                // Let the drain in progress finish for a bounded time; if it
                // does not, dropping it is the induced-crash case the
                // durable queue already covers.
                if let Ok(outcome) = tokio::time::timeout(settings.shutdown_grace, &mut drain).await {
                    record(&mut report, outcome);
                }
                return report;
            }
            outcome = &mut drain => outcome,
        };
        let pause = match record(&mut report, outcome) {
            Pause::None => continue,
            Pause::Idle => settings.idle,
            Pause::Error => settings.error_backoff,
        };
        tokio::select! {
            biased;
            () = &mut shutdown => return report,
            () = tokio::time::sleep(pause) => {}
        }
    }
}

enum Pause {
    None,
    Idle,
    Error,
}

fn record(
    report: &mut ServiceReport,
    outcome: Result<crate::worker::WorkerReport, Error>,
) -> Pause {
    match outcome {
        Ok(drained) => {
            report.drains = report.drains.saturating_add(1);
            report.claimed = report.claimed.saturating_add(drained.claimed);
            report.completed = report.completed.saturating_add(drained.completed);
            report.failed = report.failed.saturating_add(drained.failed);
            report.dead = report.dead.saturating_add(drained.dead);
            report.quarantined = report.quarantined.saturating_add(drained.quarantined);
            if drained.claimed == 0 {
                Pause::Idle
            } else {
                Pause::None
            }
        }
        Err(error) => {
            report.store_errors = report.store_errors.saturating_add(1);
            report.last_error = Some(error.to_string());
            Pause::Error
        }
    }
}
