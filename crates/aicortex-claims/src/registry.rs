//! The registered vocabularies, as a value (spec 050 B-9, FR-005).
//!
//! A [`RegistrySnapshot`] is every registered [`PredicateSet`] a caller has
//! read, keyed by namespace and version. It answers two questions and does
//! no I/O: whether a new document may be registered ([`RegistrySnapshot::check`])
//! and what a [`PredicateRef`] names ([`RegistrySnapshot::predicate`]). The
//! store's registry repository reads the rows, builds a snapshot, asks it,
//! and stages the insert; this module is the rule, that one is the table.
//!
//! The rule (B-9, I-3): a registered version is immutable, so registering it
//! again with identical content changes nothing and with different content
//! is refused. A new version must follow the namespace's latest, and it may
//! add subject kinds, predicates, enumeration variants and decimal units,
//! and may deprecate a predicate. It may not change an existing predicate's
//! value type or cardinality, and it may not drop a predicate, a subject
//! kind, a variant or a unit, because claims written under the earlier
//! version must keep their meaning (050 D-11).

use core::fmt;
use std::collections::BTreeMap;

use aicortex_types::{Namespace, PredicateDef, PredicateRef, PredicateSet};

/// What registering a document would do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Admission {
    /// A new version: stage it and append its Decision (B-15).
    New,
    /// The same version with identical content: a no-op (FR-005).
    Unchanged,
}

/// Why a document cannot be registered (FR-005).
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum RegistryError {
    /// The namespace is reserved for aicortex's own vocabulary (B-9).
    ReservedNamespace,
    /// The version is registered with different content (I-3).
    ConflictingVersion {
        /// The version.
        version: u32,
    },
    /// The version does not follow the namespace's latest.
    NotAfterLatest {
        /// The version offered.
        version: u32,
        /// The latest registered.
        latest: u32,
    },
    /// A later version changes what an earlier one said (I-3).
    Incompatible {
        /// The predicate or subject kind concerned.
        subject: String,
        /// What changed, as a stable code.
        change: Change,
    },
}

/// What an incompatible later version changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    /// A predicate's value type changed, including dropping a variant or a
    /// unit.
    ValueType,
    /// A predicate's cardinality or slot changed.
    Cardinality,
    /// A predicate was dropped rather than deprecated.
    PredicateRemoved,
    /// A subject kind was dropped.
    SubjectKindRemoved,
}

impl Change {
    /// The stable code.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::ValueType => "value_type_changed",
            Self::Cardinality => "cardinality_changed",
            Self::PredicateRemoved => "predicate_removed",
            Self::SubjectKindRemoved => "subject_kind_removed",
        }
    }
}

impl RegistryError {
    /// The stable code a fixture and a Decision name.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::ReservedNamespace => "reserved_namespace",
            Self::ConflictingVersion { .. } => "conflicting_version",
            Self::NotAfterLatest { .. } => "not_after_latest",
            Self::Incompatible { change, .. } => change.code(),
        }
    }
}

impl fmt::Display for RegistryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ReservedNamespace => {
                write!(f, "the namespace {:?} is reserved", Namespace::RESERVED)
            }
            Self::ConflictingVersion { version } => {
                write!(f, "version {version} is registered with different content")
            }
            Self::NotAfterLatest { version, latest } => {
                write!(f, "version {version} does not follow the latest, {latest}")
            }
            Self::Incompatible { subject, change } => {
                write!(f, "{subject}: {}", change.code())
            }
        }
    }
}

impl core::error::Error for RegistryError {}

/// Every registered document a caller has read (B-9).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RegistrySnapshot {
    sets: BTreeMap<(Namespace, u32), PredicateSet>,
}

impl RegistrySnapshot {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Build a snapshot from registered documents, in any order.
    ///
    /// # Errors
    ///
    /// The [`RegistryError`] of the first document that could not have been
    /// registered after the ones before it in version order: a stored
    /// registry that breaks its own rule is reported, not repaired.
    pub fn from_sets(sets: impl IntoIterator<Item = PredicateSet>) -> Result<Self, RegistryError> {
        let mut ordered: Vec<PredicateSet> = sets.into_iter().collect();
        ordered.sort_by(|a, b| (a.namespace(), a.version()).cmp(&(b.namespace(), b.version())));
        let mut snapshot = Self::new();
        for set in ordered {
            snapshot.admit(set)?;
        }
        Ok(snapshot)
    }

    /// Whether `set` may be registered (FR-005).
    ///
    /// # Errors
    ///
    /// [`RegistryError`] naming the rule it breaks.
    pub fn check(&self, set: &PredicateSet) -> Result<Admission, RegistryError> {
        if set.namespace().is_reserved() {
            return Err(RegistryError::ReservedNamespace);
        }
        if let Some(existing) = self.sets.get(&(set.namespace().clone(), set.version())) {
            return if existing == set {
                Ok(Admission::Unchanged)
            } else {
                Err(RegistryError::ConflictingVersion {
                    version: set.version(),
                })
            };
        }
        if let Some(latest) = self.latest(set.namespace()) {
            if set.version() < latest.version() {
                return Err(RegistryError::NotAfterLatest {
                    version: set.version(),
                    latest: latest.version(),
                });
            }
            successor(latest, set)?;
        }
        Ok(Admission::New)
    }

    /// Check `set` and, when it is new, add it.
    ///
    /// # Errors
    ///
    /// As [`Self::check`].
    pub fn admit(&mut self, set: PredicateSet) -> Result<Admission, RegistryError> {
        let admission = self.check(&set)?;
        if admission == Admission::New {
            self.sets
                .insert((set.namespace().clone(), set.version()), set);
        }
        Ok(admission)
    }

    /// The highest registered version of `namespace`.
    #[must_use]
    pub fn latest(&self, namespace: &Namespace) -> Option<&PredicateSet> {
        self.sets
            .range((namespace.clone(), 0)..=(namespace.clone(), u32::MAX))
            .next_back()
            .map(|(_, set)| set)
    }

    /// The registered document at `namespace` and `version`.
    #[must_use]
    pub fn set(&self, namespace: &Namespace, version: u32) -> Option<&PredicateSet> {
        self.sets.get(&(namespace.clone(), version))
    }

    /// What `predicate` names, at the version it names.
    #[must_use]
    pub fn predicate(&self, predicate: &PredicateRef) -> Option<&PredicateDef> {
        self.set(&predicate.namespace, predicate.version)
            .and_then(|set| set.predicate(&predicate.name))
    }

    /// Every registered document, by namespace then version.
    pub fn sets(&self) -> impl Iterator<Item = &PredicateSet> {
        self.sets.values()
    }
}

/// Whether `later` may follow `earlier` in the same namespace (I-3).
fn successor(earlier: &PredicateSet, later: &PredicateSet) -> Result<(), RegistryError> {
    let incompatible =
        |subject: String, change| Err(RegistryError::Incompatible { subject, change });
    for kind in earlier.subject_kinds() {
        if !later.subject_kinds().contains(kind) {
            return incompatible(kind.to_string(), Change::SubjectKindRemoved);
        }
    }
    for old in earlier.predicates() {
        let Some(new) = later.predicate(old.name()) else {
            return incompatible(old.name().to_string(), Change::PredicateRemoved);
        };
        if new.cardinality() != old.cardinality() {
            return incompatible(old.name().to_string(), Change::Cardinality);
        }
        let (was, is) = (old.value(), new.value());
        let keeps_units = was.units().iter().all(|unit| is.units().contains(unit));
        let keeps_variants = was
            .variants()
            .iter()
            .all(|variant| is.variants().contains(variant));
        if was.kind() != is.kind() || !keeps_units || !keeps_variants {
            return incompatible(old.name().to_string(), Change::ValueType);
        }
    }
    Ok(())
}
