//! The record itself (spec 011 B-3, B-4, B-6, B-9, B-10, B-11).
//!
//! The predecessor stored text, a vector, and a free-form JSON bag whose
//! shape lived inside a prompt, so no two rows agreed and no query could rely
//! on a field existing (`openbrain://json-bag-schema`). Here the shape is
//! types: a closed taxonomy a model cannot widen by emitting a new word, a
//! status that says whether the claim is still true, and a version stamped on
//! every row so a migration reinterprets old records explicitly rather than
//! by assumption.

use core::fmt;
use core::str::FromStr;

use rahi_types::UnixSeconds;
use serde::ser::SerializeStruct;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::actor::Actor;
use crate::error::{Result, TypeError, unknown_variant, validate_key};
use crate::id::MemoryId;
use crate::provenance::Provenance;
use crate::scope::Scope;
use crate::trust::TrustClass;

/// The version every record written by this code carries (B-11).
///
/// It is bumped by the spec that changes the shape, in the same change as the
/// migration that reinterprets the older value.
pub const MEMORY_SCHEMA_VERSION: u16 = 1;

/// The vocabulary of [`MemoryKind`], for the refusal message of FR-002.
const MEMORY_KINDS: &[&str] = &[
    "observation",
    "fact",
    "preference",
    "decision",
    "task",
    "reference",
    "person_note",
    "correction",
];

/// The vocabulary of [`Status`], for the refusal message of FR-002.
const STATUSES: &[&str] = &["active", "quarantined", "superseded", "expired", "erased"];

/// What kind of claim a memory is (B-4).
///
/// Closed. A string outside this list is a deserialization error naming
/// `kind`, not a passthrough, so the model that proposes metadata fills a
/// fixed structure instead of inventing one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MemoryKind {
    /// Something seen: what happened, without a claim about what it means.
    Observation,
    /// A claim about the world that is expected to stay true.
    Fact,
    /// How the subject wants things done.
    Preference,
    /// A choice that was made, and which therefore has consequences.
    Decision,
    /// Something to be done.
    Task,
    /// A pointer to something that lives elsewhere.
    Reference,
    /// Something about a person the subject works with.
    PersonNote,
    /// A claim that corrects an earlier one.
    Correction,
}

impl MemoryKind {
    /// The discriminant, as spec 012 B-2 stores it in the `kind` column.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Observation => "observation",
            Self::Fact => "fact",
            Self::Preference => "preference",
            Self::Decision => "decision",
            Self::Task => "task",
            Self::Reference => "reference",
            Self::PersonNote => "person_note",
            Self::Correction => "correction",
        }
    }

    /// Every kind, in the order the vocabulary is written.
    #[must_use]
    pub const fn all() -> [Self; 8] {
        [
            Self::Observation,
            Self::Fact,
            Self::Preference,
            Self::Decision,
            Self::Task,
            Self::Reference,
            Self::PersonNote,
            Self::Correction,
        ]
    }
}

impl FromStr for MemoryKind {
    type Err = TypeError;

    fn from_str(text: &str) -> Result<Self> {
        match text {
            "observation" => Ok(Self::Observation),
            "fact" => Ok(Self::Fact),
            "preference" => Ok(Self::Preference),
            "decision" => Ok(Self::Decision),
            "task" => Ok(Self::Task),
            "reference" => Ok(Self::Reference),
            "person_note" => Ok(Self::PersonNote),
            "correction" => Ok(Self::Correction),
            other => Err(unknown_variant("kind", other, MEMORY_KINDS)),
        }
    }
}

impl fmt::Display for MemoryKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

impl Serialize for MemoryKind {
    fn serialize<S: Serializer>(&self, serializer: S) -> core::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(self.label())
    }
}

impl<'de> Deserialize<'de> for MemoryKind {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> core::result::Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Self::from_str(&text).map_err(serde::de::Error::custom)
    }
}

/// Whether a memory is still true, and if not, what became of it (B-6).
///
/// Only [`Status::Active`] is visible to retrieval by default. The transitions
/// between these are spec 014's; what lives here is the shape they move
/// through.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Status {
    /// True, and visible to retrieval.
    Active,
    /// Stored, but its origin could not be established (spec 013 B-7). It
    /// waits for a human or a curator and is invisible to retrieval.
    Quarantined,
    /// Replaced by a later memory, which is named.
    Superseded(MemoryId),
    /// Past the moment it was true until.
    Expired,
    /// Forgotten. The body is gone, not blanked with a marker.
    Erased,
}

impl Status {
    /// The discriminant, as spec 012 B-2 stores it in the `status` column.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Quarantined => "quarantined",
            Self::Superseded(_) => "superseded",
            Self::Expired => "expired",
            Self::Erased => "erased",
        }
    }

    /// Whether retrieval sees this memory without being asked to widen.
    #[must_use]
    pub const fn is_retrievable(self) -> bool {
        matches!(self, Self::Active)
    }

    /// The memory that replaced this one, when one did.
    #[must_use]
    pub const fn superseded_by(self) -> Option<MemoryId> {
        match self {
            Self::Superseded(id) => Some(id),
            Self::Active | Self::Quarantined | Self::Expired | Self::Erased => None,
        }
    }
}

/// The wire shape of a [`Status`]: the state, then the successor when the
/// state names one.
#[derive(Deserialize)]
struct StatusWire {
    state: String,
    #[serde(default)]
    by: Option<MemoryId>,
}

impl<'de> Deserialize<'de> for Status {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> core::result::Result<Self, D::Error> {
        let wire = StatusWire::deserialize(deserializer)?;
        match wire.state.as_str() {
            "active" => Ok(Self::Active),
            "quarantined" => Ok(Self::Quarantined),
            "superseded" => {
                let by = wire
                    .by
                    .ok_or(TypeError::MissingField {
                        field: "status.by",
                        context: "a superseded memory",
                    })
                    .map_err(serde::de::Error::custom)?;
                Ok(Self::Superseded(by))
            }
            "expired" => Ok(Self::Expired),
            "erased" => Ok(Self::Erased),
            other => Err(serde::de::Error::custom(unknown_variant(
                "status", other, STATUSES,
            ))),
        }
    }
}

impl Serialize for Status {
    fn serialize<S: Serializer>(&self, serializer: S) -> core::result::Result<S::Ok, S::Error> {
        match self.superseded_by() {
            None => {
                let mut status = serializer.serialize_struct("Status", 1)?;
                status.serialize_field("state", self.label())?;
                status.end()
            }
            Some(by) => {
                let mut status = serializer.serialize_struct("Status", 2)?;
                status.serialize_field("state", self.label())?;
                status.serialize_field("by", &by)?;
                status.end()
            }
        }
    }
}

/// A media object referenced by content hash (B-10).
///
/// The bytes live outside the row. What the record carries is the digest, so
/// two memories that reference the same object reference the same digest, and
/// the erasure of spec 014 can tell whether anything still points at it.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct MediaRef {
    /// `sha256:<64 lowercase hex>`, the digest shape the chassis's ledger
    /// already uses, so one reader parses both.
    pub digest: MediaDigest,
    /// The IANA media type of the object.
    pub media_type: String,
    /// The object's size, when it is known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
}

impl MediaRef {
    /// Reference an object by digest and media type.
    ///
    /// # Errors
    ///
    /// [`TypeError::Invalid`] naming `media.media_type` when the type is
    /// empty, padded, over the ceiling, or carries a control character.
    pub fn new(digest: MediaDigest, media_type: impl Into<String>) -> Result<Self> {
        let media_type = media_type.into();
        validate_key("media.media_type", &media_type)?;
        Ok(Self {
            digest,
            media_type,
            bytes: None,
        })
    }

    /// The same reference, naming the object's size.
    #[must_use]
    pub const fn with_bytes(mut self, bytes: u64) -> Self {
        self.bytes = Some(bytes);
        self
    }
}

/// A `sha256:<64 lowercase hex>` content digest.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct MediaDigest(String);

impl MediaDigest {
    /// The prefix every digest carries, so the value is self-describing.
    const PREFIX: &'static str = "sha256:";
    /// The number of hex characters in a sha256 digest.
    const HEX_LEN: usize = 64;

    /// Parse a `sha256:<64 lowercase hex>` digest.
    ///
    /// # Errors
    ///
    /// [`TypeError::Invalid`] naming `media.digest` when the prefix is absent
    /// or the body is not 64 lowercase hex characters.
    pub fn parse(digest: impl Into<String>) -> Result<Self> {
        let digest = digest.into();
        let invalid = |reason: &str| TypeError::Invalid {
            field: "media.digest",
            reason: format!("{digest:?} {reason}"),
        };
        let Some(body) = digest.strip_prefix(Self::PREFIX) else {
            return Err(invalid("lacks the sha256: prefix"));
        };
        if body.len() != Self::HEX_LEN {
            return Err(invalid("is not 64 hex characters after the prefix"));
        }
        if !body
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(invalid("is not lowercase hex after the prefix"));
        }
        Ok(Self(digest))
    }

    /// The digest text, prefix included.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for MediaDigest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for MediaDigest {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> core::result::Result<Self, D::Error> {
        let digest = String::deserialize(deserializer)?;
        Self::parse(digest).map_err(serde::de::Error::custom)
    }
}

/// What the memory says (B-10).
///
/// Text is the canonical form. Everything this product derives, from the
/// embedding of spec 015 to the entities of spec 017, is derived from `text`
/// and not from the media, so a memory whose media is unreachable is still a
/// memory.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct MemoryBody {
    /// The canonical content.
    pub text: String,
    /// A short name, when one was given or derived.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Objects referenced by digest, which live outside the row.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub media: Vec<MediaRef>,
}

impl MemoryBody {
    /// A body that is only text.
    #[must_use]
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            title: None,
            media: Vec::new(),
        }
    }

    /// The same body, with a title.
    #[must_use]
    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// The same body, referencing an object.
    #[must_use]
    pub fn with_media(mut self, media: MediaRef) -> Self {
        self.media.push(media);
        self
    }

    /// The length of the canonical content in bytes, which is what the size
    /// ceilings of spec 012 B-8 and spec 013 B-6 are measured against.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.text.len()
    }

    /// Whether there is any canonical content at all. Spec 013 B-6 refuses a
    /// body that normalizes to this.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.text.is_empty()
    }
}

/// How much a memory matters, and a decay that is computed rather than
/// written (B-9).
///
/// Nothing rewrites a row to age it. `base` is set when the memory is written
/// and moved only by a deliberate act; the passage of time is applied at read
/// time by [`Importance::decayed_at`]. A background job that rewrote every
/// row every night would turn a read-mostly store into a write-heavy one and
/// would make `updated` meaningless.
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd, Serialize)]
pub struct Importance {
    /// The undecayed weight, in `0.0..=1.0`.
    base: f32,
    /// When the memory was last recalled or written.
    last_used: UnixSeconds,
    /// How often it has been recalled.
    uses: u32,
}

impl Importance {
    /// The half-life of importance: thirty days, in seconds.
    ///
    /// Spec 018 B-? owns the recency half-life used in ranking; this one is
    /// the record's own, and spec 040 measures a change to either.
    pub const HALF_LIFE_SECS: u64 = 30 * 24 * 60 * 60;

    /// Weigh a memory.
    ///
    /// # Errors
    ///
    /// [`TypeError::Invalid`] naming `importance.base` when the weight is not
    /// a number in `0.0..=1.0`. The range is not decoration: `decayed_at` is
    /// only monotonically non-increasing (FR-005) because `base` cannot be
    /// negative.
    pub fn new(base: f32, last_used: UnixSeconds, uses: u32) -> Result<Self> {
        // `contains` is false for NaN and for either infinity, so the one
        // check covers every value the range does not admit.
        if !(0.0..=1.0).contains(&base) {
            return Err(TypeError::Invalid {
                field: "importance.base",
                reason: format!("{base} is not a number in 0.0..=1.0"),
            });
        }
        Ok(Self {
            base,
            last_used,
            uses,
        })
    }

    /// The default weight of a memory nobody has weighed: half, as of now,
    /// used once by being written.
    ///
    /// # Errors
    ///
    /// Never in practice: the constant is in range. The signature keeps the
    /// one validating constructor as the only way in.
    pub fn at(now: UnixSeconds) -> Result<Self> {
        Self::new(0.5, now, 1)
    }

    /// The undecayed weight.
    #[must_use]
    pub const fn base(self) -> f32 {
        self.base
    }

    /// When the memory was last recalled or written.
    #[must_use]
    pub const fn last_used(self) -> UnixSeconds {
        self.last_used
    }

    /// How often it has been recalled.
    #[must_use]
    pub const fn uses(self) -> u32 {
        self.uses
    }

    /// The weight as of `now`, after half-life decay since `last_used`.
    ///
    /// Monotonically non-increasing in elapsed time, and exactly `base` at
    /// zero elapsed time (FR-005). A `now` before `last_used` is not an error
    /// and not a negative elapsed time: clocks move, and the answer is `base`.
    ///
    /// `clippy::float_arithmetic` is denied across the workspace (spec 010
    /// B-1) and is allowed here, on this function alone, because the decay
    /// B-9 requires is a floating-point computation and there is no integer
    /// spelling of it that is easier to verify. See spec 011 D-4: the
    /// enumeration in 010 B-1 predates this requirement and wants a human
    /// amendment.
    #[expect(
        clippy::float_arithmetic,
        reason = "B-9's decay is floating point; the allow is one function wide (D-4)"
    )]
    #[must_use]
    pub fn decayed_at(self, now: UnixSeconds) -> f32 {
        let elapsed = now.get().saturating_sub(self.last_used.get());
        if elapsed == 0 {
            return self.base;
        }
        let half_lives = elapsed as f32 / Self::HALF_LIFE_SECS as f32;
        self.base * 0.5_f32.powf(half_lives)
    }

    /// The same weight, recalled once more at `now`.
    #[must_use]
    pub const fn used_at(mut self, now: UnixSeconds) -> Self {
        self.last_used = now;
        self.uses = self.uses.saturating_add(1);
        self
    }
}

/// The wire shape of an [`Importance`], validated on the way in so a stored
/// row cannot reintroduce a base the constructor would have refused.
#[derive(Deserialize)]
struct ImportanceWire {
    base: f32,
    last_used: UnixSeconds,
    uses: u32,
}

impl<'de> Deserialize<'de> for Importance {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> core::result::Result<Self, D::Error> {
        let wire = ImportanceWire::deserialize(deserializer)?;
        Self::new(wire.base, wire.last_used, wire.uses).map_err(serde::de::Error::custom)
    }
}

/// Everything [`Memory::new`] needs, as a struct literal that cannot be
/// written without a [`Provenance`].
///
/// This is the builder B-8 permits: one that cannot finish without provenance
/// because the field is required and there is no `Default`.
#[derive(Clone, Debug, PartialEq)]
pub struct MemoryParts {
    /// The identifier the writer minted.
    pub id: MemoryId,
    /// The scope the memory lives in.
    pub scope: Scope,
    /// What kind of claim it is.
    pub kind: MemoryKind,
    /// What it says.
    pub body: MemoryBody,
    /// Who made the claim.
    pub actor: Actor,
    /// Where it came from. Required, with no default and no way around it.
    pub provenance: Provenance,
    /// What a client may do with it.
    pub trust: TrustClass,
    /// How much it matters.
    pub importance: Importance,
    /// When it was written.
    pub created: UnixSeconds,
}

/// One memory (B-3).
///
/// `#[non_exhaustive]` closes the struct literal to other crates, so outside
/// this one the only way to a `Memory` is [`Memory::new`], which takes a
/// [`MemoryParts`] whose `provenance` field is required, or deserialization,
/// which has no default for it either. That is the whole of FR-004, and it is
/// checked by the compiler rather than by a test that somebody has to run.
///
/// A struct literal is not a way around it:
///
/// ```compile_fail
/// # use aicortex_types::{Memory, MemoryBody, MemoryKind};
/// # fn build(parts: aicortex_types::MemoryParts) -> Memory {
/// // `Memory` is #[non_exhaustive]: no literal outside the defining crate.
/// Memory { body: MemoryBody::text("x"), ..Memory::new(parts) }
/// # }
/// ```
///
/// Nor is a default:
///
/// ```compile_fail
/// # use aicortex_types::Memory;
/// // There is no `Default for Memory`, and there will not be one: a default
/// // memory is a memory with no provenance.
/// let _ = Memory::default();
/// ```
///
/// Nor is an incomplete parts literal:
///
/// ```compile_fail
/// # use aicortex_types::{Memory, MemoryParts};
/// # fn build(other: MemoryParts) -> Memory {
/// Memory::new(MemoryParts {
///     id: other.id,
///     scope: other.scope,
///     kind: other.kind,
///     body: other.body,
///     actor: other.actor,
///     // provenance omitted
///     trust: other.trust,
///     importance: other.importance,
///     created: other.created,
/// })
/// # }
/// ```
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Memory {
    /// The identifier the writer minted (B-1).
    pub id: MemoryId,
    /// The scope it lives in (B-2).
    pub scope: Scope,
    /// What kind of claim it is (B-4).
    pub kind: MemoryKind,
    /// What it says (B-10).
    pub body: MemoryBody,
    /// Who made the claim (B-7).
    pub actor: Actor,
    /// Where it came from (B-8). Required on every path.
    pub provenance: Provenance,
    /// What a client may do with it (B-5).
    pub trust: TrustClass,
    /// Whether it is still true (B-6).
    pub status: Status,
    /// How much it matters (B-9).
    pub importance: Importance,
    /// The version this record was written under (B-11).
    pub schema_version: u16,
    /// When it was written.
    pub created: UnixSeconds,
    /// When it last changed.
    pub updated: UnixSeconds,
}

impl Memory {
    /// Write a memory: active, at the current schema version, unchanged since
    /// it was created.
    #[must_use]
    pub fn new(parts: MemoryParts) -> Self {
        Self {
            id: parts.id,
            scope: parts.scope,
            kind: parts.kind,
            body: parts.body,
            actor: parts.actor,
            provenance: parts.provenance,
            trust: parts.trust,
            status: Status::Active,
            importance: parts.importance,
            schema_version: MEMORY_SCHEMA_VERSION,
            created: parts.created,
            updated: parts.created,
        }
    }

    /// Whether retrieval sees this memory (B-6).
    #[must_use]
    pub const fn is_retrievable(&self) -> bool {
        self.status.is_retrievable()
    }

    /// Whether a client may act on this memory's content (B-5).
    ///
    /// False for everything but an instruction-grade memory, which only a
    /// human decision can produce.
    #[must_use]
    pub const fn is_actionable(&self) -> bool {
        self.trust.is_actionable()
    }
}
