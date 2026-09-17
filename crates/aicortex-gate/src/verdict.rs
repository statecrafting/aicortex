//! What the gate answers, and what a refusal is allowed to say (spec 013
//! B-2, B-3, B-9).
//!
//! Two properties are carried by the types here rather than by a convention.
//!
//! **An admitted memory cannot be forged.** [`Admitted`] has a private field,
//! no public constructor, no `Deserialize`, and no `From`. The only values of
//! it in existence are the ones [`crate::Gate::evaluate`] put inside a
//! [`Verdict`], which is what makes `MemoryRepo::insert` taking one equivalent
//! to "this went through the gate" (B-1). Spec 013 FR-004 is the compile-fail
//! case that holds it.
//!
//! **A refusal carries no content.** [`Reason`] is a closed enum of codes,
//! counts, offsets and detector names. There is no variant with a body
//! excerpt, no `Display` that interpolates one, and no serialization that
//! reaches the candidate. FR-002 asserts it by searching a serialized refusal
//! for the fixture's secret.

use aicortex_types::{Memory, MemoryId, Scope, Status};
use rahi_types::Sub;
use serde::Serialize;

use crate::candidate::Origin;
use crate::rules::DetectorId;

/// A memory that passed the gate.
///
/// The only way to one is a [`Verdict`]. The field is private and the type
/// has no constructor, so a caller outside this crate can hold one, read it,
/// and hand it to the store, and can never make one.
#[derive(Clone, Debug, PartialEq)]
pub struct Admitted {
    memory: Memory,
}

impl Admitted {
    /// Wrap a memory the gate has just cleared. Crate-private: this function
    /// is the admission boundary, and it has exactly two call sites, both in
    /// [`crate::Gate`].
    pub(crate) const fn new(memory: Memory) -> Self {
        Self { memory }
    }

    /// The memory, normalized, as it will be stored.
    #[must_use]
    pub const fn memory(&self) -> &Memory {
        &self.memory
    }

    /// The memory, taken out of the verdict.
    #[must_use]
    pub fn into_memory(self) -> Memory {
        self.memory
    }
}

/// Why a candidate was refused or quarantined (B-2).
///
/// Closed, with a stable code and a human sentence. The code is what a
/// Decision, a fixture and a client all name; the sentence is for a person
/// reading a log.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum Reason {
    /// A detector recognised a credential (B-3, B-4).
    SecretDetected {
        /// Which detector fired.
        detector: DetectorId,
        /// The byte offset into the normalized text where the value starts.
        /// The offset, never the value.
        offset: usize,
    },
    /// The normalized body is over the text ceiling (B-6).
    TooLarge {
        /// How many bytes the normalized body carries.
        bytes: usize,
        /// The ceiling it passed.
        ceiling: usize,
    },
    /// Normalization left nothing behind (B-6).
    EmptyAfterNormalization,
    /// The origin could not be established, or could only be asserted (B-7).
    OriginUnestablished {
        /// How much the caller could show for the source.
        origin: Origin,
    },
    /// The media references are more than the ceiling, or of a type this
    /// deployment does not store (B-6).
    UnsupportedMedia(MediaFault),
    /// The caller is over a rate limit.
    ///
    /// The gate never produces this: rate limiting belongs to the chassis
    /// (`rahi://020`, `rahi://025`) and is out of this spec's scope. The code
    /// is in the closed enum so that a surface refusing for that reason
    /// ledgers it in the same vocabulary as every other refusal, rather than
    /// inventing a second one.
    RateExceeded {
        /// How many captures the window allows.
        limit: u32,
        /// How long the window is, in seconds.
        window_secs: u32,
    },
    /// A deployment policy refuses this candidate outright.
    PolicyDenied {
        /// Which policy, by its stable name.
        policy: String,
    },
}

/// What is wrong with a candidate's media (B-6).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "fault", rename_all = "snake_case")]
pub enum MediaFault {
    /// More references than the ceiling allows.
    TooMany {
        /// How many the candidate carries.
        count: usize,
        /// The ceiling it passed.
        ceiling: usize,
    },
    /// A reference this deployment does not store.
    ///
    /// The position, not the type: a media type is caller-supplied text, and
    /// a refusal that echoed caller-supplied text back would be a channel for
    /// exactly the content FR-002 forbids.
    UnsupportedType {
        /// Which reference, by position in the body's media list.
        index: usize,
    },
}

impl Reason {
    /// The stable code, as a Decision, a fixture and a client name it.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::SecretDetected { .. } => "secret_detected",
            Self::TooLarge { .. } => "too_large",
            Self::EmptyAfterNormalization => "empty_after_normalization",
            Self::OriginUnestablished { .. } => "origin_unestablished",
            Self::UnsupportedMedia(_) => "unsupported_media",
            Self::RateExceeded { .. } => "rate_exceeded",
            Self::PolicyDenied { .. } => "policy_denied",
        }
    }

    /// The human sentence, which names the shape of the problem and never the
    /// content of it.
    #[must_use]
    pub fn sentence(&self) -> String {
        match self {
            Self::SecretDetected { detector, offset } => format!(
                "the {detector} detector matched at byte {offset}; a credential is refused, not redacted"
            ),
            Self::TooLarge { bytes, ceiling } => {
                format!("the normalized body is {bytes} bytes, over the {ceiling} byte ceiling")
            }
            Self::EmptyAfterNormalization => {
                "the body is empty once normalized, so there is nothing to remember".to_owned()
            }
            Self::OriginUnestablished { origin } => format!(
                "the origin is {}, which this deployment will not admit as an established source",
                origin.label()
            ),
            Self::UnsupportedMedia(MediaFault::TooMany { count, ceiling }) => format!(
                "the candidate references {count} media objects, over the ceiling of {ceiling}"
            ),
            Self::UnsupportedMedia(MediaFault::UnsupportedType { index }) => {
                format!("media reference {index} is of a type this deployment does not store")
            }
            Self::RateExceeded { limit, window_secs } => {
                format!("the caller is over {limit} captures per {window_secs} seconds")
            }
            Self::PolicyDenied { policy } => {
                format!("deployment policy {policy} refuses this candidate")
            }
        }
    }
}

impl core::fmt::Display for Reason {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}: {}", self.code(), self.sentence())
    }
}

/// What the gate answers (B-2).
#[derive(Clone, Debug, PartialEq)]
pub enum Verdict {
    /// Store it.
    Admit(Admitted),
    /// Store it out of sight, and say why (B-7).
    Quarantine(Admitted, Reason),
    /// Store nothing.
    Refuse(Reason),
}

impl Verdict {
    /// The memory this verdict admits, if it admits one.
    #[must_use]
    pub const fn admitted(&self) -> Option<&Admitted> {
        match self {
            Self::Admit(admitted) | Self::Quarantine(admitted, _) => Some(admitted),
            Self::Refuse(_) => None,
        }
    }

    /// The reason, when there is one. An `Admit` has none.
    #[must_use]
    pub const fn reason(&self) -> Option<&Reason> {
        match self {
            Self::Admit(_) => None,
            Self::Quarantine(_, reason) | Self::Refuse(reason) => Some(reason),
        }
    }

    /// Whether this verdict stores a row at all.
    #[must_use]
    pub const fn stores_a_row(&self) -> bool {
        self.admitted().is_some()
    }

    /// The status the stored row carries, when one is stored.
    #[must_use]
    pub const fn status(&self) -> Option<Status> {
        match self {
            Self::Admit(_) => Some(Status::Active),
            Self::Quarantine(..) => Some(Status::Quarantined),
            Self::Refuse(_) => None,
        }
    }
}

/// The keyed digest of a Decision, as B-9 requires it to be carried.
///
/// Three fields, no fourth: the digest, the algorithm that produced it, and
/// the id of the key that is held in the application store. The key itself is
/// not here, the content is not here, and there is no method that recovers
/// either. Erasing the key row (spec 014) leaves this value opaque, which is
/// what lets an append-only chain and constitution XIII both hold.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DigestRef {
    /// The digest, lowercase hex.
    pub digest: String,
    /// The algorithm identifier, `HMAC-SHA-256` today.
    pub algorithm: String,
    /// The id of the key row that produced it.
    pub key_id: String,
}

/// The decision kind a refusal is appended under.
pub const KIND_REFUSE: &str = "memory.gate.refuse";

/// The decision kind a quarantine is appended under.
pub const KIND_QUARANTINE: &str = "memory.gate.quarantine";

/// The decision kind an operator override is appended under.
pub const KIND_OVERRIDE: &str = "memory.gate.override";

/// Everything a caller needs to append the Decision of B-9, and nothing else.
///
/// The gate does not link rahi's ledger, because it is pure and the ledger is
/// I/O. What it owns is the *shape* of the record, so that the surface of
/// spec 020, the importer of spec 030 and this crate's own tests all append
/// the same fields rather than three approximations of them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LedgerEntry {
    /// The decision kind: one of [`KIND_REFUSE`], [`KIND_QUARANTINE`],
    /// [`KIND_OVERRIDE`].
    pub kind: &'static str,
    /// Whether the outcome is a denial. A quarantine is not: the memory was
    /// stored, out of sight.
    pub denied: bool,
    /// The subject the Decision names.
    pub actor: Sub,
    /// The human sentence, which carries no content.
    pub reason: String,
    /// The reason code, the scope, the keyed digest, and the memory id when a
    /// row was stored.
    pub payload: serde_json::Value,
}

/// The Decision B-9 requires for a refusal or a quarantine.
///
/// `memory` is `Some` exactly when a row was stored, which is what lets spec
/// 014's erasure find the key row that covers it.
#[must_use]
pub fn ledger_entry(
    kind: &'static str,
    reason: &Reason,
    scope: &Scope,
    actor: &Sub,
    digest: &DigestRef,
    memory: Option<MemoryId>,
) -> LedgerEntry {
    LedgerEntry {
        kind,
        denied: kind == KIND_REFUSE,
        actor: actor.clone(),
        reason: reason.sentence(),
        payload: serde_json::json!({
            "reason_code": reason.code(),
            "reason": reason,
            "scope": scope.to_string(),
            "digest": digest,
            "memory_id": memory.map(|id| id.to_string()),
        }),
    }
}
