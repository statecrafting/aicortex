//! What the cell hands the source adapter framework (spec 030 B-9, D-10).
//!
//! The runner, the registry of sources, and the deployment's intake policy.
//! No concrete adapter exists in spec 030, so the registry is empty and a
//! run request for any name is a 404. A concrete adapter (031, 032, 033) adds
//! its registration here under an `extends` edge, widens the manifest's
//! `http.egress` grants for the hosts it reaches, and constructs the `ingest`
//! service's facade, which registration holds each declared host to (FR-004).

#![forbid(unsafe_code)]

use std::sync::Arc;

use aicortex_ingest::{IngestPolicy, Registry, Runner};
use rahi_types::{EnvReader, Error};

/// The sources this build can run.
#[must_use]
pub fn registry() -> Registry {
    Registry::new()
}

/// The deployment's intake policy, from its environment.
///
/// # Errors
///
/// [`Error::Config`] for a value that is neither choice of
/// [`IngestPolicy::PUBLIC_ORIGIN_ENV`]: a typo must not decide whether a
/// fetched page is trusted, so `serve` refuses to start on it.
pub fn policy<E: EnvReader + ?Sized>(env: &E) -> Result<IngestPolicy, Error> {
    IngestPolicy::from_env(env)
}

/// The runner the routes drive.
///
/// An unreadable policy falls back to quarantining public sources, the safe
/// side; `serve` has already refused to start on one ([`policy`]), and
/// `aicortex preflight` reports it.
#[must_use]
pub fn runner<E: EnvReader + ?Sized>(env: &E) -> Runner {
    Runner::new(Arc::new(registry()), None, policy(env).unwrap_or_default())
}
