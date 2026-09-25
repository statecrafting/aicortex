//! A claim checked against a domain's registered vocabulary (spec 050 B-14).
//!
//! This crate is the seam a consumer can link without a store: the claim
//! types live in `aicortex-types`, the registered vocabularies arrive as a
//! [`RegistrySnapshot`] the caller has read, and [`validate`] is a pure
//! function of the two. It has no clock, no randomness, no I/O, no SQL and
//! no async (AC-4). Spec 052 adds history and projection beside it.
//!
//! An unknown predicate is refused, never stored as free text (I-2, 017 B-5's
//! rule applied here). Counting those refusals belongs to the caller that
//! observes them, since a pure function keeps no tally;
//! [`ClaimError::code`] is the stable label a counter keys on.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod registry;

use core::fmt;

use aicortex_types::{
    Cardinality, Claim, ClaimValue, PredicateDef, PredicateRef, Stance, ValueKind,
};
use unicode_normalization::is_nfc;

pub use registry::{Admission, Change, RegistryError, RegistrySnapshot};

/// Why a claim does not validate (FR-004).
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ClaimError {
    /// No registered version of the namespace declares the predicate.
    UnknownPredicate(PredicateRef),
    /// The namespace declares the predicate, but not at the named version.
    WrongVersion(PredicateRef),
    /// The named version deprecates the predicate: it takes no new claims.
    Withdrawn(PredicateRef),
    /// The subject's namespace or kind is not one the predicate applies to.
    WrongSubjectKind {
        /// The subject as given.
        subject: String,
    },
    /// The value's type is not the declared one.
    WrongValueType {
        /// The declared type.
        expected: ValueKind,
        /// The value's type.
        found: ValueKind,
    },
    /// A decimal's unit is not one the predicate admits.
    UndeclaredUnit {
        /// The unit given, or `None` for a unitless number.
        unit: Option<String>,
    },
    /// An enumeration variant the predicate does not declare.
    UndeclaredVariant {
        /// The variant given.
        variant: String,
    },
    /// A `many` predicate's claim without a slot.
    MissingSlot,
    /// A `one` predicate's claim with a slot.
    UnexpectedSlot,
    /// A stance the predicate does not admit.
    InadmissibleStatus {
        /// The stance given.
        stance: Stance,
    },
    /// Text that is not in Unicode normalization form C (013 B-8).
    TextNotNormalized,
    /// A span points into a memory the provenance does not derive from
    /// (B-2).
    SpanOutsideDerivation,
}

impl ClaimError {
    /// The stable code a fixture, a counter and a Decision name.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::UnknownPredicate(_) => "unknown_predicate",
            Self::WrongVersion(_) => "wrong_version",
            Self::Withdrawn(_) => "withdrawn_predicate",
            Self::WrongSubjectKind { .. } => "wrong_subject_kind",
            Self::WrongValueType { .. } => "wrong_value_type",
            Self::UndeclaredUnit { .. } => "undeclared_unit",
            Self::UndeclaredVariant { .. } => "undeclared_variant",
            Self::MissingSlot => "missing_slot",
            Self::UnexpectedSlot => "unexpected_slot",
            Self::InadmissibleStatus { .. } => "inadmissible_status",
            Self::TextNotNormalized => "text_not_normalized",
            Self::SpanOutsideDerivation => "span_outside_derivation",
        }
    }
}

impl fmt::Display for ClaimError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownPredicate(predicate) => write!(f, "{predicate} is not registered"),
            Self::WrongVersion(predicate) => {
                write!(f, "{predicate} names a version that does not declare it")
            }
            Self::Withdrawn(predicate) => write!(f, "{predicate} is deprecated at that version"),
            Self::WrongSubjectKind { subject } => {
                write!(f, "the predicate does not apply to {subject}")
            }
            Self::WrongValueType { expected, found } => {
                write!(f, "the predicate takes {expected}, not {found}")
            }
            Self::UndeclaredUnit { unit } => match unit {
                Some(unit) => write!(f, "the predicate does not admit the unit {unit}"),
                None => f.write_str("the predicate requires a unit"),
            },
            Self::UndeclaredVariant { variant } => {
                write!(f, "the predicate does not declare the variant {variant}")
            }
            Self::MissingSlot => f.write_str("the predicate has cardinality many and needs a slot"),
            Self::UnexpectedSlot => {
                f.write_str("the predicate has cardinality one and takes no slot")
            }
            Self::InadmissibleStatus { stance } => {
                write!(f, "the predicate does not admit the stance {stance}")
            }
            Self::TextNotNormalized => f.write_str("the text is not in normalization form C"),
            Self::SpanOutsideDerivation => {
                f.write_str("a span points into a memory the claim is not derived from")
            }
        }
    }
}

impl core::error::Error for ClaimError {}

/// A claim that [`validate`] accepted, with the definition it was checked
/// against. Only [`validate`] builds one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidClaim {
    claim: Claim,
    predicate: PredicateDef,
}

impl ValidClaim {
    /// The claim.
    #[must_use]
    pub const fn claim(&self) -> &Claim {
        &self.claim
    }

    /// The predicate definition it was checked against.
    #[must_use]
    pub const fn predicate(&self) -> &PredicateDef {
        &self.predicate
    }

    /// The claim, by value.
    #[must_use]
    pub fn into_claim(self) -> Claim {
        self.claim
    }
}

/// Check `claim` against the registered vocabularies (B-14).
///
/// # Errors
///
/// The first [`ClaimError`] found, in this order: the predicate is
/// registered at the named version and not withdrawn; the subject is of a
/// kind the predicate applies to; the value matches the declared type, unit
/// and variant set; the slot is present exactly when cardinality is `many`;
/// the stance is admitted; text is NFC; and every span points into a
/// memory the provenance derives from.
pub fn validate(claim: &Claim, registry: &RegistrySnapshot) -> Result<ValidClaim, ClaimError> {
    let reference = &claim.predicate;
    let Some(def) = registry.predicate(reference) else {
        let declared_elsewhere = registry.sets().any(|set| {
            set.namespace() == &reference.namespace && set.predicate(&reference.name).is_some()
        });
        return Err(if declared_elsewhere {
            ClaimError::WrongVersion(reference.clone())
        } else {
            ClaimError::UnknownPredicate(reference.clone())
        });
    };
    if def.deprecated() {
        return Err(ClaimError::Withdrawn(reference.clone()));
    }
    if claim.subject.namespace != reference.namespace
        || !def.subjects().contains(&claim.subject.kind)
    {
        return Err(ClaimError::WrongSubjectKind {
            subject: claim.subject.to_string(),
        });
    }
    check_value(&claim.value, def)?;
    match (def.cardinality(), &claim.slot) {
        (Cardinality::Many(_), None) => return Err(ClaimError::MissingSlot),
        (Cardinality::One, Some(_)) => return Err(ClaimError::UnexpectedSlot),
        _ => {}
    }
    let stance = claim.epistemic.stance();
    if !def.epistemic().contains(&stance) {
        return Err(ClaimError::InadmissibleStatus { stance });
    }
    let value_text = match &claim.value {
        ClaimValue::Text(text) => Some(text.as_str()),
        _ => None,
    };
    if value_text
        .into_iter()
        .chain(claim.epistemic.condition())
        .any(|text| !is_nfc(text))
    {
        return Err(ClaimError::TextNotNormalized);
    }
    let provenance = &claim.provenance;
    if provenance
        .spans
        .iter()
        .any(|span| !provenance.derived_from.contains(&span.source))
    {
        return Err(ClaimError::SpanOutsideDerivation);
    }
    Ok(ValidClaim {
        claim: claim.clone(),
        predicate: def.clone(),
    })
}

fn check_value(value: &ClaimValue, def: &PredicateDef) -> Result<(), ClaimError> {
    let declared = def.value();
    if value.kind() != declared.kind() {
        return Err(ClaimError::WrongValueType {
            expected: declared.kind(),
            found: value.kind(),
        });
    }
    match value {
        ClaimValue::Decimal(decimal) => {
            let admitted = match decimal.unit() {
                Some(unit) => declared.units().contains(unit),
                None => declared.units().is_empty(),
            };
            if !admitted {
                return Err(ClaimError::UndeclaredUnit {
                    unit: decimal.unit().map(ToString::to_string),
                });
            }
        }
        ClaimValue::Enum(variant) if !declared.variants().contains(variant) => {
            return Err(ClaimError::UndeclaredVariant {
                variant: variant.to_string(),
            });
        }
        _ => {}
    }
    Ok(())
}
