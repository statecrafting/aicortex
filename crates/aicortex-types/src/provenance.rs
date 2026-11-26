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

use rahi_types::{Sub, UnixSeconds};
use serde::{Deserialize, Deserializer, Serialize};

use crate::error::{Result, TypeError, validate_key};
use crate::id::MemoryId;
use crate::span::SourceSpan;
use crate::trust::DecisionRef;

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

/// The record of an operator admitting a candidate the write gate refused
/// (spec 013 B-5, an additive extension of this file).
///
/// The gate refuses credentials rather than redacting them, and a detector
/// with no false positive is a detector that misses. B-5's answer is an
/// operator-authenticated override per candidate, and this is the mark it
/// leaves on the memory that was admitted anyway: which refusal was
/// overridden, who overrode it, and the decision in rahi's ledger that
/// records the act. A memory carrying one is a memory a reviewer can find.
///
/// There is no variant of this for "the gate was off". There is no way to
/// turn the gate off.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct AdmissionOverride {
    /// The stable reason code of the refusal that was overridden.
    pub reason_code: String,
    /// The human who overrode it. Agents do not have a subject of their own,
    /// so an agent cannot override its own refusal, for the same reason it
    /// cannot promote its own memory.
    pub by: Sub,
    /// The decision in rahi's ledger that records the override.
    pub decision: DecisionRef,
}

impl AdmissionOverride {
    /// Record an override of the refusal `reason_code` by `by`, as `decision`
    /// in the ledger.
    #[must_use]
    pub const fn new(reason_code: String, by: Sub, decision: DecisionRef) -> Self {
        Self {
            reason_code,
            by,
            decision,
        }
    }
}

/// The identifier a source gives a conversation (spec 031 B-9).
///
/// The export's own id, or the derived `d:` id 031 B-9 defines. Two message
/// memories are turns of one conversation exactly when they share a scope, a
/// source system, and this id. The value is held to the shape of every other
/// key in this crate (non-empty, unpadded, no control character, at most 128
/// bytes) so it is safe as a SQL key and as a label.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct ConversationId(String);

impl ConversationId {
    /// Validate and wrap a conversation id.
    ///
    /// # Errors
    ///
    /// [`TypeError::Invalid`] naming `conversation.id` when the id is empty,
    /// padded, over the length ceiling, or carries a control character.
    pub fn new(id: impl Into<String>) -> Result<Self> {
        let id = id.into();
        validate_key("conversation.id", &id)?;
        Ok(Self(id))
    }

    /// The id.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ConversationId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for ConversationId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> core::result::Result<Self, D::Error> {
        let id = String::deserialize(deserializer)?;
        Self::new(id).map_err(serde::de::Error::custom)
    }
}

/// Where a message memory sits in its conversation (spec 031 B-9).
///
/// The wire form is the provenance key `conversation` holding an object with
/// exactly the keys `id` and `turn`; an extra, missing, or renamed key is
/// refused. `turn` is the message's 1-based position in the export's message
/// order, assigned before skipping, so a skipped message leaves a gap rather
/// than renumbering the turns after it.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "ConversationRefWire")]
pub struct ConversationRef {
    /// The conversation the message belongs to.
    pub id: ConversationId,
    /// The message's 1-based turn number.
    pub turn: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConversationRefWire {
    id: ConversationId,
    turn: u32,
}

impl TryFrom<ConversationRefWire> for ConversationRef {
    type Error = TypeError;

    fn try_from(wire: ConversationRefWire) -> Result<Self> {
        Self::new(wire.id, wire.turn)
    }
}

impl ConversationRef {
    /// Name a turn of a conversation.
    ///
    /// # Errors
    ///
    /// [`TypeError::Invalid`] naming `conversation.turn` when `turn` is 0:
    /// turns are numbered from 1.
    pub fn new(id: ConversationId, turn: u32) -> Result<Self> {
        if turn == 0 {
            return Err(TypeError::Invalid {
                field: "conversation.turn",
                reason: "is 0, and turns are numbered from 1".to_owned(),
            });
        }
        Ok(Self { id, turn })
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
    /// The operator override that admitted this memory past a refusal, when
    /// one did (spec 013 B-5).
    ///
    /// Additive and absent from every record that was simply admitted, so a
    /// memory written before this field existed reads back unchanged and the
    /// record schema version does not move.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admission: Option<AdmissionOverride>,
    /// Where in the stored sources the claim was read (spec 050 B-13).
    ///
    /// Additive and serialized only when non-empty, so every record written
    /// before this field existed reads back unchanged (011 FR-001). A span
    /// carries offsets and a keyed digest, never content.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub spans: Vec<SourceSpan>,
    /// The conversation this message memory is a turn of, when it is one
    /// (spec 031 B-9, added here under spec 025 section 2).
    ///
    /// Additive and serialized only when present, so every record written
    /// before this field existed reads back byte-identically (011 FR-001).
    /// A memory is conversational exactly when this is set (025 B-2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation: Option<ConversationRef>,
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
            admission: None,
            spans: Vec::new(),
            conversation: None,
        }
    }

    /// The same provenance, marked as admitted by an operator override
    /// (spec 013 B-5).
    #[must_use]
    pub fn overridden(mut self, admission: AdmissionOverride) -> Self {
        self.admission = Some(admission);
        self
    }

    /// The same provenance, naming the memories this one came from and the
    /// extractor that did the deriving.
    #[must_use]
    pub fn derived(mut self, derived_from: Vec<MemoryId>, extractor: ExtractorVersion) -> Self {
        self.derived_from = derived_from;
        self.extractor = Some(extractor);
        self
    }

    /// The same provenance, pointing into its sources (spec 050 B-12).
    #[must_use]
    pub fn with_spans(mut self, spans: Vec<SourceSpan>) -> Self {
        self.spans = spans;
        self
    }

    /// The same provenance, naming the conversation turn it is (031 B-9).
    #[must_use]
    pub fn in_conversation(mut self, conversation: ConversationRef) -> Self {
        self.conversation = Some(conversation);
        self
    }

    /// Whether this memory was derived from others.
    #[must_use]
    pub const fn is_derived(&self) -> bool {
        !self.derived_from.is_empty()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn provenance(conversation: &str) -> String {
        format!(r#"{{"source":{{"system":"chat"}},"captured_at":1,"ingested_at":2{conversation}}}"#)
    }

    #[test]
    fn a_conversation_reference_has_exactly_the_keys_id_and_turn() {
        let wire = provenance(r#","conversation":{"id":"c1","turn":4}"#);
        let parsed: Provenance = serde_json::from_str(&wire).unwrap();
        let reference = parsed.conversation.clone().unwrap();
        assert_eq!((reference.id.as_str(), reference.turn), ("c1", 4));
        assert_eq!(serde_json::to_string(&parsed).unwrap(), wire);

        for bad in [
            r#","conversation":{"id":"c1"}"#,
            r#","conversation":{"turn":4}"#,
            r#","conversation":{"id":"c1","turn":4,"extra":1}"#,
            r#","conversation":{"id":"c1","step":4}"#,
            r#","conversation":{"id":"c1","turn":0}"#,
            r#","conversation":{"id":"","turn":1}"#,
        ] {
            assert!(
                serde_json::from_str::<Provenance>(&provenance(bad)).is_err(),
                "{bad} was accepted"
            );
        }
    }

    #[test]
    fn a_provenance_without_a_conversation_serializes_without_the_key() {
        let wire = provenance("");
        let parsed: Provenance = serde_json::from_str(&wire).unwrap();
        assert_eq!(parsed.conversation, None);
        assert_eq!(serde_json::to_string(&parsed).unwrap(), wire);
    }
}
