//! The operator verbs of spec 015 B-9, as plain functions.
//!
//! The application mounts them on its operator surface, which the chassis
//! puts behind the operator role. Each takes the scope it acts on
//! explicitly: the pipeline holds no scope inventory (D-17, D-18), so an
//! operator names the scope, and a pass over many scopes is many calls.

use rahi_kernel::{Egress, Governed, Manifest};
use rahi_store::StoreHandle;
use rahi_types::{Error, Sub, UnixSeconds};

use crate::activation::{Activation, activate_provider};
use crate::config::EmbeddingConfig;
use crate::registry::ModelRegistry;
use crate::remote::EmbeddingTransport;
use crate::worker::{MAX_REEMBED_BATCH, ReembeddingBatch, stage_reembedding_batch};

/// Check the configuration against the manifest ceiling, resolve the
/// provider, and make its model the active revision.
///
/// The ceiling is checked first, so a remote provider whose host is absent
/// from the manifest is refused before any artifact is read or any row is
/// written (B-6).
///
/// # Errors
///
/// [`Error::Config`] when no provider is configured; [`Error::Denied`] from
/// the ceiling check; artifact, egress, or store errors from boot and
/// activation.
pub async fn activate_configured<T: EmbeddingTransport>(
    config: &EmbeddingConfig,
    manifest: &Manifest,
    egress: Option<&Governed<Egress>>,
    actor: &Sub,
    transport: T,
    store: &StoreHandle,
    now: UnixSeconds,
) -> Result<Activation, Error> {
    config.check(manifest)?;
    let provider = config
        .boot(egress, actor, transport)
        .await?
        .ok_or_else(|| Error::Config("no embedding provider is configured".to_owned()))?;
    activate_provider(store, &provider, now).await
}

/// Stage one bounded re-embedding pass over `scope_id` for the active
/// revision, resuming after `cursor`.
///
/// Repeat with the returned cursor while `more` is true. The pass is
/// idempotent: staging a memory that already has its job is a no-op.
///
/// # Errors
///
/// [`Error::Conflict`] when no model is active; [`Error::Validation`] for a
/// cursor that is not a memory id; store errors; [`Error::Config`] for a
/// limit outside `1..=MAX_REEMBED_BATCH`.
pub async fn reembed_scope(
    store: &StoreHandle,
    scope_id: &str,
    cursor: Option<&str>,
    limit: u32,
    now: UnixSeconds,
) -> Result<ReembeddingBatch, Error> {
    let active = ModelRegistry::active(store)
        .await?
        .ok_or_else(|| Error::Conflict("no embedding model is active".to_owned()))?;
    let cursor = cursor
        .map(|raw| {
            raw.parse()
                .map_err(|_| Error::Validation(format!("cursor {raw:?} is not a memory id")))
        })
        .transpose()?;
    stage_reembedding_batch(
        store,
        scope_id,
        &active,
        cursor,
        limit.min(MAX_REEMBED_BATCH),
        now,
    )
    .await
}

/// Drop one inactive revision's vectors and chunks for `scope_id`, once the
/// active revision covers every live memory in it.
///
/// # Errors
///
/// [`Error::Conflict`] when the revision is active, unknown, or coverage is
/// incomplete; store errors.
pub async fn drop_revision(
    store: &StoreHandle,
    scope_id: &str,
    revision: u32,
) -> Result<(), Error> {
    ModelRegistry::drop_revision(store, scope_id, revision).await
}
