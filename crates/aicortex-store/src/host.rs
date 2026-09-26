//! Public composition contracts for hosts that link Aicortex as a library.
//!
//! The host owns the Rahi cell and every transaction. This module supplies
//! Aicortex's immutable named migration set and the operator configuration
//! value required to authorize predicate vocabulary registration.

use aicortex_claims::RegistryError;
use aicortex_types::Namespace;
use rahi_store::MigrationSet;
use rahi_types::{Error, Sub};

use crate::migrations;

/// The immutable migration-set name a host records for Aicortex.
pub const AICORTEX_MIGRATION_SET_NAME: &str = "aicortex";

/// Aicortex's eight migrations under their library identity.
///
/// # Errors
///
/// Returns Rahi's validation error if the fixed set name or requirements no
/// longer satisfy the released migration-set contract.
pub fn migration_set() -> Result<MigrationSet, Error> {
    MigrationSet::new(AICORTEX_MIGRATION_SET_NAME, migrations().to_vec())?
        .requires("rahi.receipts", 1)?
        .requires("rahi.coordination", 1)
}

/// One exact predicate document an operator permits the host to install.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PredicateRegistrationGrant {
    namespace: Namespace,
    version: u32,
    document_digest: String,
}

impl PredicateRegistrationGrant {
    /// Grant one namespace, version, and canonical document digest.
    #[must_use]
    pub fn new(namespace: Namespace, version: u32, document_digest: impl Into<String>) -> Self {
        Self {
            namespace,
            version,
            document_digest: document_digest.into(),
        }
    }

    /// The granted namespace.
    #[must_use]
    pub const fn namespace(&self) -> &Namespace {
        &self.namespace
    }

    /// The granted version.
    #[must_use]
    pub const fn version(&self) -> u32 {
        self.version
    }

    /// The granted canonical document digest.
    #[must_use]
    pub fn document_digest(&self) -> &str {
        &self.document_digest
    }
}

/// Startup configuration naming the authenticated operator and their grants.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OperatorPredicateConfig {
    operator: Sub,
    grants: Vec<PredicateRegistrationGrant>,
}

impl OperatorPredicateConfig {
    /// Build non-empty operator configuration.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] when no document is granted.
    pub fn new(operator: Sub, grants: Vec<PredicateRegistrationGrant>) -> Result<Self, Error> {
        if grants.is_empty() {
            return Err(Error::Validation(
                "operator predicate configuration must grant at least one document".into(),
            ));
        }
        Ok(Self { operator, grants })
    }

    /// The authenticated operator represented by this configuration.
    #[must_use]
    pub const fn operator(&self) -> &Sub {
        &self.operator
    }

    /// The exact documents this operator permits.
    #[must_use]
    pub fn grants(&self) -> &[PredicateRegistrationGrant] {
        &self.grants
    }

    pub(crate) fn grant(
        &self,
        namespace: &Namespace,
        version: u32,
    ) -> Option<&PredicateRegistrationGrant> {
        self.grants
            .iter()
            .find(|grant| grant.namespace == *namespace && grant.version == version)
    }
}

/// The stable identity of a validated predicate registration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PredicateRegistrationIdentity {
    namespace: Namespace,
    version: u32,
    document_digest: String,
}

impl PredicateRegistrationIdentity {
    pub(crate) fn new(namespace: Namespace, version: u32, document_digest: String) -> Self {
        Self {
            namespace,
            version,
            document_digest,
        }
    }

    /// The registered namespace.
    #[must_use]
    pub const fn namespace(&self) -> &Namespace {
        &self.namespace
    }

    /// The registered version.
    #[must_use]
    pub const fn version(&self) -> u32 {
        self.version
    }

    /// The canonical document digest.
    #[must_use]
    pub fn document_digest(&self) -> &str {
        &self.document_digest
    }
}

/// A validated insertion. Only the registry repository can construct it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthorizedPredicateRegistration {
    pub(crate) identity: PredicateRegistrationIdentity,
    pub(crate) operator: Sub,
    pub(crate) document: String,
}

/// The complete result of pure registration validation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PredicateRegistrationPlan {
    /// Stage this operator-authorized insertion.
    Insert(AuthorizedPredicateRegistration),
    /// The identical document is already registered, so stage nothing.
    Unchanged(PredicateRegistrationIdentity),
}

/// Why operator-authorized predicate registration was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PredicateRegistrationRefusal {
    /// No grant names the document's exact namespace and version.
    NotOperatorGranted {
        /// The ungranted namespace.
        namespace: Namespace,
        /// The ungranted version.
        version: u32,
    },
    /// The grant does not name the canonical bytes being offered.
    GrantDigestMismatch {
        /// The digest in operator configuration.
        granted: String,
        /// The digest of the canonical document bytes.
        actual: String,
    },
    /// This namespace and version already hold different content.
    ConflictingVersion {
        /// The conflicting namespace.
        namespace: Namespace,
        /// The conflicting version.
        version: u32,
    },
    /// Another predicate-registry rule refused the document.
    Registry(RegistryError),
    /// Canonical JSON serialization failed.
    Serialization(String),
}

impl core::fmt::Display for PredicateRegistrationRefusal {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NotOperatorGranted { namespace, version } => {
                write!(
                    formatter,
                    "operator configuration does not grant {namespace}@{version}"
                )
            }
            Self::GrantDigestMismatch { granted, actual } => write!(
                formatter,
                "operator grant digest {granted} does not match canonical digest {actual}"
            ),
            Self::ConflictingVersion { namespace, version } => {
                write!(
                    formatter,
                    "{namespace}@{version} is registered with different content"
                )
            }
            Self::Registry(error) => error.fmt(formatter),
            Self::Serialization(error) => {
                write!(formatter, "predicate set does not serialize: {error}")
            }
        }
    }
}

impl core::error::Error for PredicateRegistrationRefusal {}
