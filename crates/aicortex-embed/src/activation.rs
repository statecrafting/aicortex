//! First activation and model change (spec 015 B-8, B-9, D-7).
//!
//! The provider resolved at boot decides the revision. The first provider
//! becomes revision 1. The same identity (model, dimensions, normalization)
//! keeps the active revision, so a restart is a no-op. Any other identity is
//! a new revision numbered after the latest ever recorded, never a reused
//! number, so durable work and stored vectors keep their meaning. Old
//! vectors stay queryable under their own revision until an operator drops
//! it, and a re-embedding pass (a separate, per-scope step) fills the new
//! one.

use rahi_store::{StoreHandle, TxnBuilder};
use rahi_types::{Error, UnixSeconds};

use crate::provider::EmbeddingProvider;
use crate::registry::{ModelRegistry, ModelRevision};

/// The outcome of activating the configured provider.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Activation {
    /// The revision now active.
    pub model: ModelRevision,
    /// Whether this call made a revision active: `true` on first activation
    /// and on a model change, `false` when the provider already was.
    pub changed: bool,
}

/// Make the provider's model the active revision.
///
/// # Errors
///
/// Store errors. Another node may activate between the reads and the commit;
/// the commit is then refused, never merged: `embedding_model.revision` is the
/// primary key, the registry's conflict clause deactivates nothing and
/// violates `active NOT NULL` for a different identity, and the one-active
/// index refuses a second active row. The caller reads again and retries.
pub async fn activate_provider(
    store: &StoreHandle,
    provider: &impl EmbeddingProvider,
    now: UnixSeconds,
) -> Result<Activation, Error> {
    if let Some(active) = ModelRegistry::active(store).await?
        && active.model_id == provider.id()
        && active.dims == provider.dims()
        && active.normalized == provider.normalized()
    {
        return Ok(Activation {
            model: active,
            changed: false,
        });
    }
    let revision = ModelRegistry::latest_revision(store)
        .await?
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| Error::Integrity("model revision numbers are exhausted".to_owned()))?;
    let model = ModelRevision::from_provider(provider, revision, now, true)?;
    let mut txn = TxnBuilder::new();
    ModelRegistry::activate(&mut txn, &model)?;
    store.txn(txn.into_statements()).await?;
    Ok(Activation {
        model,
        changed: true,
    })
}
