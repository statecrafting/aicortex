//! Trust class, and the token that is the only way to reach the top of it
//! (spec 011 B-5, D-1).
//!
//! Three classes decide what a client may do with a memory. `Evidence` is
//! something that happened and was recorded. `Assertion` is somebody's claim.
//! `Instruction` is a memory a client may act on, and it is the dangerous
//! one: a system that lets an agent mark its own output as instruction grade
//! has handed the agent the ability to write its own orders.
//!
//! So `Instruction` is not checked, it is unconstructible. The variant is
//! `#[non_exhaustive]`, so no crate but this one can name it; the only way to
//! obtain one is [`TrustClass::instruction`], which demands a [`Promotion`];
//! and the only way to obtain a `Promotion` is [`Promotion::new`], which
//! demands the [`Sub`] of a human and the id of a decision in rahi's ledger.
//! An agent has no `Sub` of its own (see [`crate::actor::Actor`]), so there
//! is no code path, present or future, on which it promotes its own memory.
//!
//! What this crate cannot do is confirm that the named decision exists in the
//! chain: that is a ledger read, and this crate holds no handle to anything.
//! Spec 024 owns that check at the boundary where the ledger is reachable.

use core::fmt;

use rahi_types::Sub;
use serde::ser::SerializeStruct;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::{Result, TypeError, unknown_variant, validate_key};

/// The vocabulary of [`TrustClass`], for the refusal message of FR-002.
const TRUST_CLASSES: &[&str] = &["evidence", "assertion", "instruction"];

/// The id of a decision in rahi's hash-chained ledger.
///
/// This is `rahi_ledger::DecisionId` by value rather than by type: spec 011
/// section 2 confines this crate to `rahi-types` and serde, and AC-2 refuses
/// any dependency that carries I/O or async, which `rahi-ledger` does through
/// `rahi-store` and tokio. The wire shape is the ledger's own transparent
/// string, so the conversion at the seam spec 024 owns is a rename and not a
/// re-encoding (D-2).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct DecisionRef(String);

impl DecisionRef {
    /// Validate and wrap a ledger decision id.
    ///
    /// # Errors
    ///
    /// [`TypeError::Invalid`] naming `decision` when the id is empty, padded,
    /// over the length ceiling, or carries a control character.
    pub fn new(id: impl Into<String>) -> Result<Self> {
        let id = id.into();
        validate_key("decision", &id)?;
        Ok(Self(id))
    }

    /// The id text, as the ledger wrote it.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for DecisionRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for DecisionRef {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> core::result::Result<Self, D::Error> {
        let id = String::deserialize(deserializer)?;
        Self::new(id).map_err(serde::de::Error::custom)
    }
}

/// The authority to call a memory instruction grade.
///
/// Both fields are private and there is no `Default`, so the struct literal
/// is not a way in either. [`Promotion::new`] is the only constructor, and it
/// takes a human subject: an `Actor` does not coerce to a `Sub`, so the
/// compile-fail case of FR-003 is a type error rather than a runtime check
/// somebody can forget to run.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Promotion {
    /// The human who decided. Agents do not have a subject of their own.
    by: Sub,
    /// The decision in rahi's ledger that records the act.
    decision: DecisionRef,
}

impl Promotion {
    /// Mint the authority from a human subject and the decision that records
    /// their act.
    ///
    /// A `Sub` is what rauthy issues to a person. An agent is an
    /// [`crate::actor::Actor`] with [`crate::actor::ActorKind::Agent`] and
    /// carries an [`crate::actor::ActorId`], which is a different type; it
    /// cannot be passed here.
    #[must_use]
    pub const fn new(by: Sub, decision: DecisionRef) -> Self {
        Self { by, decision }
    }

    /// The human who decided.
    #[must_use]
    pub const fn by(&self) -> &Sub {
        &self.by
    }

    /// The ledger decision that records the act.
    #[must_use]
    pub const fn decision(&self) -> &DecisionRef {
        &self.decision
    }
}

/// The wire shape of a [`Promotion`], which is also how a stored row reads
/// back.
///
/// Deserialization reconstructs a promotion a human already made; it does not
/// make one. Whether the named decision is in the chain is spec 024's check,
/// at the boundary that can reach the chain (D-2).
#[derive(Deserialize)]
struct PromotionWire {
    by: Sub,
    decision: DecisionRef,
}

impl<'de> Deserialize<'de> for Promotion {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> core::result::Result<Self, D::Error> {
        let wire = PromotionWire::deserialize(deserializer)?;
        Ok(Self::new(wire.by, wire.decision))
    }
}

impl Serialize for Promotion {
    fn serialize<S: Serializer>(&self, serializer: S) -> core::result::Result<S::Ok, S::Error> {
        let mut promotion = serializer.serialize_struct("Promotion", 2)?;
        promotion.serialize_field("by", &self.by)?;
        promotion.serialize_field("decision", &self.decision)?;
        promotion.end()
    }
}

/// What a client may do with a memory.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TrustClass {
    /// This happened and is recorded.
    Evidence,
    /// Somebody claims this is so.
    Assertion,
    /// A client may act on this.
    ///
    /// `#[non_exhaustive]` is the lock: outside this crate the variant cannot
    /// be named in an expression or a pattern without `..`, so
    /// [`TrustClass::instruction`] is the only way to one, and a `Promotion`
    /// is the only way through it.
    #[non_exhaustive]
    Instruction(Promotion),
}

impl TrustClass {
    /// Raise a memory to instruction grade on the authority of `promotion`.
    #[must_use]
    pub const fn instruction(promotion: Promotion) -> Self {
        Self::Instruction(promotion)
    }

    /// The discriminant, as spec 012 B-2 stores it in the `trust` column.
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match self {
            Self::Evidence => "evidence",
            Self::Assertion => "assertion",
            Self::Instruction(_) => "instruction",
        }
    }

    /// The authority behind an instruction-grade memory, when there is one.
    #[must_use]
    pub const fn promotion(&self) -> Option<&Promotion> {
        match self {
            Self::Evidence | Self::Assertion => None,
            Self::Instruction(promotion) => Some(promotion),
        }
    }

    /// Whether a client may act on this memory.
    #[must_use]
    pub const fn is_actionable(&self) -> bool {
        matches!(self, Self::Instruction(_))
    }
}

/// The wire shape of a [`TrustClass`]: the discriminant, then the authority
/// when the discriminant demands one.
#[derive(Deserialize)]
struct TrustWire {
    class: String,
    #[serde(default)]
    promotion: Option<Promotion>,
}

impl<'de> Deserialize<'de> for TrustClass {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> core::result::Result<Self, D::Error> {
        let wire = TrustWire::deserialize(deserializer)?;
        match wire.class.as_str() {
            "evidence" => Ok(Self::Evidence),
            "assertion" => Ok(Self::Assertion),
            "instruction" => {
                let promotion = wire
                    .promotion
                    .ok_or(TypeError::MissingField {
                        field: "trust.promotion",
                        context: "an instruction-grade memory",
                    })
                    .map_err(serde::de::Error::custom)?;
                Ok(Self::instruction(promotion))
            }
            other => Err(serde::de::Error::custom(unknown_variant(
                "trust",
                other,
                TRUST_CLASSES,
            ))),
        }
    }
}

impl Serialize for TrustClass {
    fn serialize<S: Serializer>(&self, serializer: S) -> core::result::Result<S::Ok, S::Error> {
        match self.promotion() {
            None => {
                let mut trust = serializer.serialize_struct("TrustClass", 1)?;
                trust.serialize_field("class", self.label())?;
                trust.end()
            }
            Some(promotion) => {
                let mut trust = serializer.serialize_struct("TrustClass", 2)?;
                trust.serialize_field("class", self.label())?;
                trust.serialize_field("promotion", promotion)?;
                trust.end()
            }
        }
    }
}
