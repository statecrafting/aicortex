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
/// Store errors, and [`Error::Conflict`] when another node activated between
/// the reads and the commit. The commit is then refused, never merged:
/// `embedding_model.revision` is the primary key, the registry's conflict clause deactivates nothing and
/// violates `active NOT NULL` for a different identity, and the one-active
/// index refuses a second active row. On `Conflict` the caller reads again and
/// retries. Any other error is a store failure, including the case where the
/// follow-up read that tells a lost race from a fault itself fails: the store
/// is then unreliable and the commit error is returned unchanged.
pub async fn activate_provider(
    store: &StoreHandle,
    provider: &impl EmbeddingProvider,
    now: UnixSeconds,
) -> Result<Activation, Error> {
    // The latest revision is read before the active one. A node that activates
    // in between has then either been seen (the active read returns the same
    // identity, a no-op) or it commits the very revision number this call
    // computed: the same identity converges on it, a different one is refused
    // as a conflict. Reading in the other order could mint a spurious extra
    // revision for an identity another node had just activated.
    let latest = ModelRegistry::latest_revision(store).await?;
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
    let revision = latest
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| Error::Integrity("model revision numbers are exhausted".to_owned()))?;
    let model = ModelRevision::from_provider(provider, revision, now, true)?;
    let mut txn = TxnBuilder::new();
    ModelRegistry::activate(&mut txn, &model)?;
    if let Err(error) = store.txn(txn.into_statements()).await {
        // Tell a lost race, which the caller should answer by reading again,
        // from a store failure, which it should not retry blindly.
        return Err(match ModelRegistry::latest_revision(store).await {
            Ok(Some(latest)) if latest >= revision => Error::Conflict(format!(
                "another activation committed revision {latest} first; read the registry again"
            )),
            _ => error,
        });
    }
    Ok(Activation {
        model,
        changed: true,
    })
}
