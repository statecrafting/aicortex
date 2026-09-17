//! Where the claim came from (spec 011 B-8, constitution IX).
//!
//! This module is frozen by the `invariant-freeze` constraint on spec 011: a
//! memory without provenance cannot be constructed. That is why
//! [`Provenance`] has no `Default`, why every field a claim's origin needs is
//! required rather than optional, and why there is no builder here that can
//! finish without a source and two timestamps.
//!
//! A change to this file that makes provenance skippable, by any route
//! including a serde default, breaks the constraint and is a human decision
//! recorded in the spec, never a convenience added mid-build.

use core::fmt;

use rahi_types::UnixSeconds;
use serde::{Deserialize, Deserializer, Serialize};

use crate::error::{Result, TypeError, validate_key};
use crate::id::MemoryId;

/// The system a claim came from: a client, an adapter, an importer.
///
/// An open vocabulary held to a shape rather than a closed enum. The registry
/// of legal values is the adapter registry of spec 030, which does not exist
/// yet; freezing the list here would make every new adapter an amendment to
/// this spec (D-3).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct SourceSystem(String);

impl SourceSystem {
    /// Validate and wrap a source system name.
    ///
    /// # Errors
    ///
    /// [`TypeError::Invalid`] naming `source.system` when the name is empty,
    /// padded, over the length ceiling, carries a control character, or uses
    /// a character outside `a-z`, `0-9`, `-`, `_`, `.`, and `:`. The last
    /// keeps the value usable as a path segment on the surfaces of spec 020
    /// and as a key in the SQL of spec 012.
    pub fn new(system: impl Into<String>) -> Result<Self> {
        let system = system.into();
        validate_key("source.system", &system)?;
        let legal = system.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"-_.:".contains(&byte)
        });
        if !legal {
            return Err(TypeError::Invalid {
                field: "source.system",
                reason: format!("{system:?} is not lowercase ascii with `-`, `_`, `.`, and `:`"),
            });
        }
        Ok(Self(system))
    }

    /// The name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SourceSystem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for SourceSystem {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> core::result::Result<Self, D::Error> {
        let system = String::deserialize(deserializer)?;
        Self::new(system).map_err(serde::de::Error::custom)
    }
}

/// Where a claim came from, precisely enough to go back and look.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SourceRef {
    /// The system: a client, an adapter, an importer.
    pub system: SourceSystem,
    /// The source's own id for the item, when it has one. Spec 030 B-4 makes
    /// an import idempotent on this value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_id: Option<String>,
    /// Where to look: a URI, a path, a message permalink.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locator: Option<String>,
}

impl SourceRef {
    /// A claim from `system` with nothing further known about where in it.
    #[must_use]
    pub const fn new(system: SourceSystem) -> Self {
        Self {
            system,
            external_id: None,
            locator: None,
        }
    }

    /// The same source, naming the item within the system.
    #[must_use]
    pub fn with_external_id(mut self, external_id: impl Into<String>) -> Self {
        self.external_id = Some(external_id.into());
        self
    }

    /// The same source, naming where to look.
    #[must_use]
    pub fn with_locator(mut self, locator: impl Into<String>) -> Self {
        self.locator = Some(locator.into());
        self
    }
}

impl fmt::Display for SourceRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.external_id {
            Some(external_id) => write!(f, "{}:{external_id}", self.system),
            None => write!(f, "{}", self.system),
        }
    }
}

/// The extractor that derived a memory, named and versioned.
///
/// A derived memory that does not name its extractor cannot be re-derived
/// when the extractor changes, and cannot be retired when the extractor turns
/// out to have been wrong. Spec 017 depends on this being present on every
/// derived record.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ExtractorVersion {
    /// What did the deriving.
    pub name: String,
    /// Which version of it.
    pub version: String,
}

impl ExtractorVersion {
    /// Name an extractor and its version.
    ///
    /// # Errors
    ///
    /// [`TypeError::Invalid`] naming `extractor.name` or `extractor.version`
    /// when either is empty, padded, over the ceiling, or carries a control
    /// character.
    pub fn new(name: impl Into<String>, version: impl Into<String>) -> Result<Self> {
        let name = name.into();
        let version = version.into();
        validate_key("extractor.name", &name)?;
        validate_key("extractor.version", &version)?;
        Ok(Self { name, version })
    }
}

impl fmt::Display for ExtractorVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@{}", self.name, self.version)
    }
}

/// What makes a memory a claim rather than a floating string.
///
/// There is no `Default` and no builder that finishes without a source and
/// both timestamps. [`crate::memory::Memory::new`] takes one of these by
/// value, and [`crate::memory::MemoryParts`] requires the field in the struct
/// literal, so the two construction paths out of this crate both demand it
/// and there is no third.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Provenance {
    /// Where the claim came from.
    pub source: SourceRef,
    /// When the source produced it.
    pub captured_at: UnixSeconds,
    /// When this system took it in.
    pub ingested_at: UnixSeconds,
    /// Every memory this one was derived from. Empty for an original claim.
    ///
    /// A derived memory names all of its parents, which is what lets the
    /// erasure of spec 014 reach the derivatives of an erased memory instead
    /// of leaving them behind as orphaned restatements of it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub derived_from: Vec<MemoryId>,
    /// The extractor that derived it, when it was derived.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extractor: Option<ExtractorVersion>,
}

impl Provenance {
    /// An original claim: a source and the two times.
    #[must_use]
    pub const fn captured(
        source: SourceRef,
        captured_at: UnixSeconds,
        ingested_at: UnixSeconds,
    ) -> Self {
        Self {
            source,
            captured_at,
            ingested_at,
            derived_from: Vec::new(),
            extractor: None,
        }
    }

    /// The same provenance, naming the memories this one came from and the
    /// extractor that did the deriving.
    #[must_use]
    pub fn derived(mut self, derived_from: Vec<MemoryId>, extractor: ExtractorVersion) -> Self {
        self.derived_from = derived_from;
        self.extractor = Some(extractor);
        self
    }

    /// Whether this memory was derived from others.
    #[must_use]
    pub const fn is_derived(&self) -> bool {
        !self.derived_from.is_empty()
    }
}
