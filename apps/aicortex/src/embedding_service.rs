//! The embedding pipeline's managed services (spec 015 B-2, B-3, D-25).
//!
//! `aicortex serve` owns every future returned here: it spawns each once,
//! broadcasts one cancellation when the process stops, and joins them before
//! the store closes. Nothing in this module spawns, so nothing can outlive
//! `serve` (D-25).
//!
//! Two services exist:
//!
//! - `embedding-worker`, mounted only when a provider is configured. It
//!   first installs any pinned local artifact that is absent, through the
//!   governed fetch of [`crate::embedding_fetch`] (D-32), then drains the
//!   outbox for the active model revision. It never returns
//!   before shutdown, because the chassis treats an early `Ok` as a stop
//!   (rahi spec 047 B-7); every error it meets is retried after a pause.
//! - `embedding-metrics`, always mounted. It refreshes the dead-letter,
//!   pending, and oldest-pending gauges on the chassis `/metrics` registry
//!   from deployment-wide sums of scoped reads (D-28).

#![forbid(unsafe_code)]

use std::future::Future;
use std::time::Duration;

use aicortex_embed::{
    ConfiguredProvider, EmbeddingConfig, EmbeddingPreflight, EmbeddingWorker, ModelRegistry,
    ModelRevision, NoTransport, ServiceSettings, default_chunker, default_worker_config,
    run_worker, unix_now,
};
use prometheus::{IntGauge, Opts, Registry};
use rahi_cli::{ManagedService, ServiceShutdown};
use rahi_edge::AppState;
use rahi_kernel::{CapabilityKind, Egress, Governed};
use rahi_store::StoreHandle;
use rahi_types::{EnvReader, Error, Result, Sub};

use crate::embedding_fetch::{HttpsFetcher, WeightFetch, needs_provisioning};

/// The worker's name in stop records.
pub const WORKER_NAME: &str = "embedding-worker";

/// The metrics collector's name in stop records.
pub const METRICS_NAME: &str = "embedding-metrics";

/// The actor recorded against provider boot.
const WORKER_ACTOR: &str = "aicortex-embedding-worker";

/// How often the worker re-reads the registry while it has nothing to run,
/// and while it runs, to notice a model change.
const POLL: Duration = Duration::from_secs(5);

/// The longest pause between two failed model fetches (D-32). The first
/// retry waits the worker's error backoff and each later one doubles it.
pub const FETCH_BACKOFF_CAP: Duration = Duration::from_secs(300);

/// How often the gauges are refreshed.
const METRICS_INTERVAL: Duration = Duration::from_secs(30);

/// How long one gauge refresh may walk the scope inventory.
const METRICS_BUDGET: Duration = Duration::from_secs(10);

/// Dead-letter gauge name.
pub const DEAD_LETTERS: &str = "aicortex_embedding_dead_letters";
/// Pending gauge name.
pub const PENDING: &str = "aicortex_embedding_pending";
/// Oldest-pending-age gauge name.
pub const OLDEST_PENDING_AGE: &str = "aicortex_embedding_oldest_pending_age_seconds";

/// The pacing handed to [`run_worker`]. The shutdown grace is shorter than
/// the chassis's service join bound (ten seconds) so a drain in progress is
/// finished or dropped before `serve` aborts the future.
const SETTINGS: ServiceSettings = ServiceSettings {
    idle: Duration::from_secs(2),
    error_backoff: Duration::from_secs(5),
    shutdown_grace: Duration::from_secs(5),
};

/// The three queue gauges.
#[derive(Clone, Debug)]
pub struct Gauges {
    dead: IntGauge,
    pending: IntGauge,
    oldest: IntGauge,
}

impl Gauges {
    /// Gauges that are not yet on any registry.
    ///
    /// # Errors
    ///
    /// [`Error::Config`] when a gauge cannot be built.
    pub fn new() -> Result<Self> {
        let gauge = |name: &str, help: &str| {
            IntGauge::with_opts(Opts::new(name, help)).map_err(|e| Error::Config(e.to_string()))
        };
        Ok(Self {
            dead: gauge(
                DEAD_LETTERS,
                "Embedding work that exhausted its attempts, across all scopes",
            )?,
            pending: gauge(
                PENDING,
                "Embedding work pending, claimed, or retrying, across all scopes",
            )?,
            oldest: gauge(
                OLDEST_PENDING_AGE,
                "Age in seconds of the oldest unfinished embedding work",
            )?,
        })
    }

    /// Put the gauges on `registry`.
    ///
    /// # Errors
    ///
    /// [`Error::Config`] when a name is already registered.
    pub fn register(&self, registry: &Registry) -> Result<()> {
        for gauge in [&self.dead, &self.pending, &self.oldest] {
            registry
                .register(Box::new(gauge.clone()))
                .map_err(|e| Error::Config(e.to_string()))?;
        }
        Ok(())
    }

    fn record(&self, report: &EmbeddingPreflight) {
        let to_i64 = |value: u64| i64::try_from(value).unwrap_or(i64::MAX);
        self.dead.set(to_i64(report.queue.dead));
        self.pending.set(to_i64(report.queue.pending));
        self.oldest
            .set(to_i64(report.queue.oldest_pending_age_seconds.unwrap_or(0)));
    }

    /// The current dead-letter value.
    #[must_use]
    pub fn dead(&self) -> i64 {
        self.dead.get()
    }

    /// The current pending value.
    #[must_use]
    pub fn pending(&self) -> i64 {
        self.pending.get()
    }
}

/// The services for this process.
///
/// The configuration is read once and held to the manifest ceiling before
/// anything runs, so a remote provider whose host is absent from the ceiling,
/// or a local artifact that does not match its pin, fails `serve` at boot
/// with the named error rather than at first use (B-6, FR-005). An absent
/// local artifact passes that check only when its host is granted, and then
/// the worker fetches it before binding (B-5, D-32).
///
/// # Errors
///
/// [`Error::Config`] for a malformed configuration or a metrics name
/// collision; [`Error::Denied`], [`Error::Integrity`], or [`Error::Io`] from
/// the ceiling and artifact check.
pub fn compose(
    state: &AppState,
    shutdown: &ServiceShutdown,
    env: &(impl EnvReader + Sync),
) -> Result<Vec<ManagedService>> {
    let config = EmbeddingConfig::from_env(env)?;
    config.check(state.kernel().manifest())?;
    let fetch = if needs_provisioning(&config) {
        let egress = Governed::new(
            state.kernel(),
            aicortex_embed::EMBEDDING_SERVICE,
            CapabilityKind::HttpEgress,
            "*",
            Egress,
        )?;
        Some(WeightFetch::new(egress, HttpsFetcher::new()?))
    } else {
        None
    };
    let gauges = Gauges::new()?;
    if let Some(obs) = rahi_edge::obs::current() {
        gauges.register(obs.metrics().registry())?;
    }
    let signal = shutdown.clone();
    let stop = move || {
        let signal = signal.clone();
        async move { signal.cancelled().await }
    };
    Ok(build(config, state.store().clone(), gauges, fetch, stop))
}

/// The services for a configuration, a store, and a stop signal.
///
/// `fetch` installs absent local artifacts before the worker binds; `stop`
/// makes a fresh future that resolves when the process is stopping.
pub fn build<S, Fut>(
    config: EmbeddingConfig,
    store: StoreHandle,
    gauges: Gauges,
    fetch: Option<WeightFetch>,
    stop: S,
) -> Vec<ManagedService>
where
    S: Fn() -> Fut + Clone + Send + Sync + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let mut services = vec![ManagedService::new(
        METRICS_NAME,
        collect(store.clone(), gauges, stop.clone()),
    )];
    if !matches!(config, EmbeddingConfig::Disabled) {
        services.push(ManagedService::new(
            WORKER_NAME,
            work(config, store, fetch, stop),
        ));
    }
    services
}

/// Wait `pause`, or until `stop` resolves. True when it was the stop.
async fn pause<S, Fut>(stop: &S, pause: Duration) -> bool
where
    S: Fn() -> Fut,
    Fut: Future<Output = ()>,
{
    tokio::select! {
        biased;
        () = stop() => true,
        () = tokio::time::sleep(pause) => false,
    }
}

/// Refresh the gauges until `stop` resolves.
///
/// # Errors
///
/// Never; a failed read keeps the previous values.
pub async fn collect<S, Fut>(store: StoreHandle, gauges: Gauges, stop: S) -> Result<()>
where
    S: Fn() -> Fut,
    Fut: Future<Output = ()>,
{
    loop {
        let read = EmbeddingPreflight::read_deployment(&store, unix_now(), METRICS_BUDGET);
        let outcome = tokio::select! {
            biased;
            () = stop() => return Ok(()),
            outcome = read => outcome,
        };
        // A failed read leaves the previous values in place; the next
        // refresh tries again.
        if let Ok(health) = outcome {
            gauges.record(&health.report);
        }
        if pause(&stop, METRICS_INTERVAL).await {
            return Ok(());
        }
    }
}

/// Whether the configured provider produces the vectors `active` names.
fn serves(config: &EmbeddingConfig, active: &ModelRevision) -> bool {
    config.shape().is_some_and(|shape| {
        shape.id == active.model_id
            && shape.dims == active.dims
            && shape.normalized == active.normalized
    })
}

fn same_revision(left: &ModelRevision, right: &ModelRevision) -> bool {
    left.revision == right.revision
        && left.model_id == right.model_id
        && left.dims == right.dims
        && left.normalized == right.normalized
}

/// Resolve once the registry names a different active model than `bound`.
async fn model_changed(store: &StoreHandle, bound: &ModelRevision) {
    loop {
        tokio::time::sleep(POLL).await;
        match ModelRegistry::active(store).await {
            Ok(Some(active)) if same_revision(&active, bound) => {}
            Ok(_) => return,
            Err(_) => {}
        }
    }
}

/// The worker service's whole life.
///
/// It binds to the active model revision when the configured provider
/// produces that model's vectors, runs until the process stops or the active
/// model changes, and then looks again. With no model active, or an active
/// model the configuration does not serve, it waits and polls: a fresh
/// deployment captures without jobs until an operator activates a model
/// (D-21, D-29). A remote provider is never booted, because this build links
/// no remote transport (D-27).
///
/// Before anything binds, `fetch` installs every absent pinned artifact. A
/// failed fetch is logged and retried, the pause doubling from the error
/// backoff up to [`FETCH_BACKOFF_CAP`], until it succeeds or the process
/// stops; until then the worker binds nothing and `preflight` keeps
/// warning that the artifact is absent (D-32).
pub async fn work<S, Fut>(
    config: EmbeddingConfig,
    store: StoreHandle,
    fetch: Option<WeightFetch>,
    stop: S,
) -> Result<()>
where
    S: Fn() -> Fut + Clone + Send + Sync + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    if let Some(fetch) = fetch {
        let mut backoff = SETTINGS.error_backoff;
        loop {
            let outcome = tokio::select! {
                biased;
                () = stop() => return Ok(()),
                outcome = fetch.provision(&config) => outcome,
            };
            let Err(error) = outcome else {
                break;
            };
            // A wrong pin or an ungranted host does not heal by itself, so
            // the retry slows down rather than re-downloading on a fixed beat,
            // and every failure is named where the operator reads the log.
            eprintln!(
                "{WORKER_NAME}: model fetch failed, retrying in {}s: {error}",
                backoff.as_secs()
            );
            if pause(&stop, backoff).await {
                return Ok(());
            }
            backoff = backoff.saturating_mul(2).min(FETCH_BACKOFF_CAP);
        }
    }
    let holder = format!("aicortex-embed-{}", std::process::id());
    let actor = Sub::new(WORKER_ACTOR);
    loop {
        let active = match ModelRegistry::active(&store).await {
            Ok(Some(active))
                if serves(&config, &active) && !matches!(config, EmbeddingConfig::Remote(_)) =>
            {
                Some(active)
            }
            Ok(_) | Err(_) => None,
        };
        let bound = match active {
            Some(active) => match bind(&config, &active, &actor, &holder).await {
                Ok(worker) => Some((worker, active)),
                Err(_) => None,
            },
            None => None,
        };
        let Some((worker, active)) = bound else {
            if pause(&stop, POLL).await {
                return Ok(());
            }
            continue;
        };
        let stopping = {
            let stop = stop.clone();
            let store = store.clone();
            let active = active.clone();
            async move {
                tokio::select! {
                    biased;
                    () = stop() => {}
                    () = model_changed(&store, &active) => {}
                }
            }
        };
        run_worker(&worker, &store, SETTINGS, stopping).await;
        // The run ended because the process is stopping or the model
        // changed. Only the first ends the service.
        let stopped = tokio::select! {
            biased;
            () = stop() => true,
            () = std::future::ready(()) => false,
        };
        if stopped {
            return Ok(());
        }
    }
}

async fn bind(
    config: &EmbeddingConfig,
    active: &ModelRevision,
    actor: &Sub,
    holder: &str,
) -> Result<EmbeddingWorker<ConfiguredProvider<NoTransport>>> {
    let provider = config
        .boot(None, actor, NoTransport)
        .await?
        .ok_or_else(|| Error::Config("no embedding provider is configured".to_owned()))?;
    EmbeddingWorker::new(
        provider,
        active.clone(),
        default_chunker()?,
        default_worker_config(),
        holder,
    )
}
