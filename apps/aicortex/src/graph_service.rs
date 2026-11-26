//! The extraction service (spec 017 B-7, D-5, D-10).
//!
//! One managed service, `extraction-worker`, owned by `aicortex serve` the
//! way the embedding worker is: it is spawned once, stops on the process's
//! cancellation, and never returns before it. Each pass stages extraction
//! jobs for live memories that have none at the extractor's version, then
//! drains one bounded batch under the extractor's processor, which carries
//! the same retry and dead-letter discipline as embedding. The default
//! extractor is local and deterministic (B-8, D-2), so the service opens no
//! socket and needs no egress.

#![forbid(unsafe_code)]

use std::future::Future;
use std::time::Duration;

use aicortex_embed::unix_now;
use aicortex_graph::{
    ExtractionConfig, ExtractionWorker, Extractor, PatternExtractor, schedule_deployment,
};
use rahi_cli::{ManagedService, ServiceShutdown};
use rahi_edge::AppState;
use rahi_store::StoreHandle;
use rahi_types::Result;

/// The worker's name in stop records.
pub const WORKER_NAME: &str = "extraction-worker";

/// Memories staged per scope in one scheduling pass.
const PER_SCOPE: u32 = 100;

/// The pause after a pass that found nothing to do.
const IDLE: Duration = Duration::from_secs(2);

/// The pause after a pass that failed.
const ERROR_BACKOFF: Duration = Duration::from_secs(5);

/// The services for this process.
///
/// # Errors
///
/// [`rahi_types::Error::Config`] when the worker cannot be bound.
pub fn compose(state: &AppState, shutdown: &ServiceShutdown) -> Result<Vec<ManagedService>> {
    let signal = shutdown.clone();
    let stop = move || {
        let signal = signal.clone();
        async move { signal.cancelled().await }
    };
    build(state.store().clone(), stop)
}

/// The services for a store and a stop signal.
///
/// # Errors
///
/// [`rahi_types::Error::Config`] when the worker cannot be bound.
pub fn build<S, Fut>(store: StoreHandle, stop: S) -> Result<Vec<ManagedService>>
where
    S: Fn() -> Fut + Clone + Send + Sync + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let worker = ExtractionWorker::new(
        PatternExtractor,
        ExtractionConfig::standard(),
        format!("aicortex-extract-{}", std::process::id()),
    )?;
    Ok(vec![ManagedService::new(
        WORKER_NAME,
        work(worker, store, stop),
    )])
}

/// Stage and drain until `stop` resolves.
///
/// # Errors
///
/// Never; a failed pass is retried after a pause, because the durable queue
/// makes every retry safe.
pub async fn work<E, S, Fut>(worker: ExtractionWorker<E>, store: StoreHandle, stop: S) -> Result<()>
where
    E: Extractor,
    S: Fn() -> Fut,
    Fut: Future<Output = ()>,
{
    loop {
        let pass = pass(&worker, &store);
        let busy = tokio::select! {
            biased;
            () = stop() => return Ok(()),
            outcome = pass => outcome,
        };
        let pause = match busy {
            Ok(true) => Duration::ZERO,
            Ok(false) => IDLE,
            Err(_) => ERROR_BACKOFF,
        };
        if pause.is_zero() {
            continue;
        }
        tokio::select! {
            biased;
            () = stop() => return Ok(()),
            () = tokio::time::sleep(pause) => {}
        }
    }
}

/// One scheduling pass and one drain. True when either did work.
async fn pass<E: Extractor>(worker: &ExtractionWorker<E>, store: &StoreHandle) -> Result<bool> {
    let now = unix_now();
    let staged = schedule_deployment(store, worker.extractor(), PER_SCOPE, now).await?;
    let drained = worker.drain(store, now).await?;
    Ok(staged > 0 || drained.claimed > 0)
}
