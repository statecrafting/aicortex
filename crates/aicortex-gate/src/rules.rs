//! The rule set (spec 013 B-4, spec 047 B-5).
//!
//! The detectors are action-gate's: `action_gate_core::secrets` holds the
//! prefix and armour tables, the URL, JWT, entropy and assignment detectors,
//! and the golden vectors that pin them (047 B-1). This module keeps what is
//! aicortex's own: the [`DetectorId`] a refusal names and its mapping from
//! the registry's id, the size and media ceilings, and the denied sources.
//! There is no second detector table here (047 B-5, FR-003).

use std::collections::BTreeSet;

use action_gate_core::secrets as registry;
use aicortex_types::SourceSystem;

use crate::limits::Limits;

pub use registry::{EntropyRule, SecretRules};

/// The stable name of a detector, as a refusal reports it and a fixture
/// records it.
///
/// A `&'static str` newtype rather than an enum: a detector is a table row,
/// and an enum would make adding one a code change in two places.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DetectorId(&'static str);

impl DetectorId {
    /// Name a detector.
    #[must_use]
    pub const fn new(name: &'static str) -> Self {
        Self(name)
    }

    /// The name, as it is reported and recorded.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

impl core::fmt::Display for DetectorId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.0)
    }
}

impl serde::Serialize for DetectorId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.0)
    }
}

impl DetectorId {
    /// The id a refusal names for a detector of action-gate's registry.
    ///
    /// The mapping is the identity on names, and that is the point: 013's
    /// corpus and every stored Decision name a detector by these strings, and
    /// the registry carries them unchanged (action-gate 001 B-1: an id, once
    /// shipped, is never renamed or reused). `tests/gate.rs` asserts the
    /// identity for every id the registry reports and every id the corpus
    /// records, so a rename upstream fails this crate's build rather than
    /// silently changing what a refusal says.
    #[must_use]
    pub const fn from_registry(id: registry::DetectorId) -> Self {
        Self(id.as_str())
    }
}

/// Everything the gate consults, in one value.
///
/// There is no flag here that turns the gate off, and there is no constructor
/// that produces an empty rule set (B-5). What a deployment may do is narrow
/// the limits ([`Limits::narrowed_to`]) and add denied sources; the detectors
/// are the crate's.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct RuleSet {
    /// The size and media ceilings (B-6).
    pub limits: Limits,
    /// The detectors (B-4).
    pub secrets: SecretRules,
    /// Source systems this deployment refuses outright, whatever they carry.
    ///
    /// The one place `PolicyDenied` comes from. Empty by default: a
    /// deployment that has no policy has no denials, rather than a default
    /// list somebody has to discover.
    pub denied_sources: BTreeSet<SourceSystem>,
}

impl RuleSet {
    /// The shipped rules: the full detector tables and the default ceilings.
    #[must_use]
    pub fn standard() -> Self {
        Self::default()
    }

    /// The same rules, refusing `system` outright.
    #[must_use]
    pub fn denying(mut self, system: SourceSystem) -> Self {
        self.denied_sources.insert(system);
        self
    }

    /// The same rules with narrower limits (B-6).
    ///
    /// # Errors
    ///
    /// [`crate::GateError`] when `limits` raises a ceiling rather than
    /// lowering it.
    pub fn narrowed_to(mut self, limits: Limits) -> Result<Self, crate::GateError> {
        self.limits = self.limits.narrowed_to(limits)?;
        Ok(self)
    }
}
