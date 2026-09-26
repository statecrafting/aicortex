//! The registered predicate vocabularies (spec 050 B-15, FR-005).
//!
//! A domain registers a [`PredicateSet`] version once. The document is
//! vocabulary, not memory content, so the Decision that records its
//! registration may name it by digest in the chain (B-15). The rule for what
//! may be registered is `aicortex_claims::RegistrySnapshot`'s; this file is
//! the table and the staging.
//!
//! The write follows the store's discipline: read the registry through the
//! leader ([`PredicateRegistryRepo::snapshot`]), decide, and stage the
//! insert into the caller's transaction
//! ([`PredicateRegistryRepo::stage_registration`]). The insert is a plain
//! `INSERT` on `(namespace, version)`, so two writers racing to register
//! different content under one version cannot both commit.

use aicortex_claims::{Admission, RegistrySnapshot};
use aicortex_gate::LedgerEntry;
use aicortex_types::PredicateSet;
use rahi_store::{Statement, StoreHandle, TxnBuilder, Value};
use rahi_types::{Error, UnixSeconds};
use serde::Deserialize;

use crate::hex_digest;
use crate::host::{
    AuthorizedPredicateRegistration, OperatorPredicateConfig, PredicateRegistrationIdentity,
    PredicateRegistrationPlan, PredicateRegistrationRefusal,
};
use crate::scope_repo::seconds_to_sql;

/// The decision kind a registration is appended under (B-15).
pub const KIND_REGISTER: &str = "claims.registry.register";

const INSERT_SQL: &str = "INSERT INTO predicate_registry
    (namespace, version, digest, document, registered_by, registered_at)
    VALUES ($1, $2, $3, $4, $5, $6)";

const ALL_SQL: &str = "SELECT document FROM predicate_registry ORDER BY namespace, version";

#[derive(Debug, Deserialize)]
struct Row {
    document: String,
}

/// What staging a registration did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Registration {
    /// A new version was staged. The caller commits the transaction and
    /// then appends this Decision (B-15).
    Registered(LedgerEntry),
    /// The version is registered with identical content: nothing was staged
    /// and nothing is appended (FR-005).
    Unchanged,
}

/// The registry's table.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PredicateRegistryRepo;

impl PredicateRegistryRepo {
    /// Every registered document, read through the leader so a registration
    /// decides against the latest committed state.
    ///
    /// # Errors
    ///
    /// The store's error, or [`Error::Integrity`] when a stored document does
    /// not parse or the stored versions break the registry's own rule.
    pub async fn snapshot(store: &StoreHandle) -> Result<RegistrySnapshot, Error> {
        let rows: Vec<Row> = store.query_consistent(ALL_SQL, Vec::new()).await?;
        let sets = rows
            .into_iter()
            .map(|row| {
                serde_json::from_str::<PredicateSet>(&row.document).map_err(|error| {
                    Error::Integrity(format!("a stored predicate set does not parse: {error}"))
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        RegistrySnapshot::from_sets(sets)
            .map_err(|error| Error::Integrity(format!("the stored registry: {error}")))
    }

    /// Validate `set` against the registry and exact operator configuration.
    ///
    /// This function is pure: it stages no statement and performs no I/O.
    ///
    /// # Errors
    ///
    /// A typed refusal when authority, document identity, or registry rules
    /// do not admit the registration.
    pub fn validate_registration(
        snapshot: &RegistrySnapshot,
        set: &PredicateSet,
        config: &OperatorPredicateConfig,
    ) -> Result<PredicateRegistrationPlan, PredicateRegistrationRefusal> {
        let document = serde_json::to_string(set)
            .map_err(|error| PredicateRegistrationRefusal::Serialization(error.to_string()))?;
        let digest = format!("sha256:{}", hex_digest(document.as_bytes()));
        let Some(grant) = config.grant(set.namespace(), set.version()) else {
            return Err(PredicateRegistrationRefusal::NotOperatorGranted {
                namespace: set.namespace().clone(),
                version: set.version(),
            });
        };
        if grant.document_digest() != digest {
            return Err(PredicateRegistrationRefusal::GrantDigestMismatch {
                granted: grant.document_digest().to_owned(),
                actual: digest,
            });
        }
        let identity =
            PredicateRegistrationIdentity::new(set.namespace().clone(), set.version(), digest);
        match snapshot.check(set) {
            Ok(Admission::Unchanged) => {
                return Ok(PredicateRegistrationPlan::Unchanged(identity));
            }
            Ok(Admission::New) => {}
            Err(aicortex_claims::RegistryError::ConflictingVersion { .. }) => {
                return Err(PredicateRegistrationRefusal::ConflictingVersion {
                    namespace: set.namespace().clone(),
                    version: set.version(),
                });
            }
            Err(error) => return Err(PredicateRegistrationRefusal::Registry(error)),
        }
        Ok(PredicateRegistrationPlan::Insert(
            AuthorizedPredicateRegistration {
                identity,
                operator: config.operator().clone(),
                document,
            },
        ))
    }

    /// Consume a validated plan and stage its insertion, if any.
    ///
    /// # Errors
    ///
    /// Returns a validation error only if the authorized version cannot fit
    /// the store's integer representation.
    pub fn stage_registration(
        txn: &mut TxnBuilder,
        plan: PredicateRegistrationPlan,
        at: UnixSeconds,
    ) -> Result<Registration, Error> {
        let PredicateRegistrationPlan::Insert(authorized) = plan else {
            return Ok(Registration::Unchanged);
        };
        let namespace = authorized.identity.namespace();
        let version = authorized.identity.version();
        let digest = authorized.identity.document_digest();
        let registrant = &authorized.operator;
        txn.push(Statement::with_params(
            INSERT_SQL,
            vec![
                Value::from(namespace.as_str()),
                Value::Integer(i64::from(version)),
                Value::from(digest),
                Value::from(authorized.document.as_str()),
                Value::from(registrant.as_str()),
                Value::Integer(seconds_to_sql(at)),
            ],
        ));
        Ok(Registration::Registered(LedgerEntry {
            kind: KIND_REGISTER,
            denied: false,
            actor: registrant.clone(),
            reason: format!("registered predicate set {namespace}@{version}",),
            payload: serde_json::json!({
                "namespace": namespace.as_str(),
                "version": version,
                "digest": digest,
                "registered_by": registrant.as_str(),
            }),
        }))
    }
}

/// The document digest a Decision names: `sha256:` and the lowercase hex of
/// the document's serialized bytes, which are deterministic because every
/// collection in a [`PredicateSet`] is an ordered list.
///
/// # Errors
///
/// [`Error::Validation`] when the document does not serialize.
pub fn document_digest(set: &PredicateSet) -> Result<String, Error> {
    let bytes = serde_json::to_vec(set).map_err(|error| {
        Error::Validation(format!("the predicate set does not serialize: {error}"))
    })?;
    Ok(format!("sha256:{}", hex_digest(&bytes)))
}
