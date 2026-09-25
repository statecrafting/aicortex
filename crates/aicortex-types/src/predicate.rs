//! The vocabulary a domain supplies: a namespaced, versioned predicate
//! registry document (spec 050 B-8, B-9).
//!
//! aicortex stays domain agnostic (D-3). Travel, the first consumer, writes
//! its own [`PredicateSet`]; nothing here knows what a flight segment is. A
//! document is checked for internal consistency when it is built or read
//! ([`PredicateSet::new`]); whether it may follow an earlier version of its
//! namespace is `aicortex_claims::RegistrySnapshot`'s question, because it
//! needs the earlier version.

use core::fmt;
use core::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::claim::{Namespace, Stance, SubjectKind};
use crate::claim_time::{checked_string, closed_vocabulary, lower_ident};
use crate::claim_value::{EnumVariant, Unit, ValueKind};
use crate::error::{Result, TypeError};

checked_string! {
    /// A predicate's name within its namespace: lowercase identifiers joined
    /// by `.`, such as `segment.departure`.
    PredicateName, "predicate", dotted_ident
}

fn dotted_ident(field: &'static str, text: &str) -> Result<()> {
    crate::error::validate_key(field, text)?;
    for segment in text.split('.') {
        lower_ident(field, segment).map_err(|_| TypeError::Invalid {
            field,
            reason: format!("{text:?} is not lowercase identifiers joined by `.`"),
        })?;
    }
    Ok(())
}

/// A predicate at the registry version a claim is written under:
/// `namespace:name@version` (B-9).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PredicateRef {
    /// The vocabulary.
    pub namespace: Namespace,
    /// The predicate's name in it.
    pub name: PredicateName,
    /// The registered version of the vocabulary.
    pub version: u32,
}

impl PredicateRef {
    /// Parse `namespace:name@version`.
    ///
    /// # Errors
    ///
    /// [`TypeError::Invalid`] naming `predicate` for any other shape.
    pub fn parse(text: &str) -> Result<Self> {
        let invalid = || TypeError::Invalid {
            field: "predicate",
            reason: format!("{text:?} is not namespace:name@version"),
        };
        let (namespace, rest) = text.split_once(':').ok_or_else(invalid)?;
        let (name, version) = rest.split_once('@').ok_or_else(invalid)?;
        let digits_only = !version.is_empty() && version.bytes().all(|byte| byte.is_ascii_digit());
        let version: u32 = if digits_only && !version.starts_with('0') {
            version.parse().map_err(|_| invalid())?
        } else {
            return Err(invalid());
        };
        Ok(Self {
            namespace: Namespace::new(namespace)?,
            name: PredicateName::new(name)?,
            version,
        })
    }
}

impl fmt::Display for PredicateRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}@{}", self.namespace, self.name, self.version)
    }
}

impl FromStr for PredicateRef {
    type Err = TypeError;

    fn from_str(text: &str) -> Result<Self> {
        Self::parse(text)
    }
}

impl Serialize for PredicateRef {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> core::result::Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for PredicateRef {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> core::result::Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Self::parse(&text).map_err(serde::de::Error::custom)
    }
}

checked_string! {
    /// The name of the key a `many` predicate's slots are told apart by, such
    /// as `traveler` (B-4).
    SlotName, "slot", lower_ident
}

/// The value type a predicate declares (B-8): a [`ValueKind`], with the
/// admitted units of a decimal and the variants of an enumeration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "RawValueType", into = "RawValueType")]
pub struct ValueType {
    kind: ValueKind,
    units: Vec<Unit>,
    variants: Vec<EnumVariant>,
}

impl ValueType {
    /// A type with no units and no variants.
    ///
    /// # Errors
    ///
    /// [`TypeError::MissingField`] naming `variants` for [`ValueKind::Enum`],
    /// which needs [`Self::enumeration`].
    pub fn plain(kind: ValueKind) -> Result<Self> {
        Self::build(kind, Vec::new(), Vec::new())
    }

    /// A decimal admitting exactly `units`; empty admits only a unitless
    /// number.
    ///
    /// # Errors
    ///
    /// [`TypeError::Invalid`] naming `units` when one is listed twice.
    pub fn decimal(units: Vec<Unit>) -> Result<Self> {
        Self::build(ValueKind::Decimal, units, Vec::new())
    }

    /// An enumeration of `variants`.
    ///
    /// # Errors
    ///
    /// [`TypeError::Invalid`] naming `variants` when empty or repeated.
    pub fn enumeration(variants: Vec<EnumVariant>) -> Result<Self> {
        Self::build(ValueKind::Enum, Vec::new(), variants)
    }

    fn build(kind: ValueKind, units: Vec<Unit>, variants: Vec<EnumVariant>) -> Result<Self> {
        if !units.is_empty() && kind != ValueKind::Decimal {
            return Err(TypeError::Invalid {
                field: "units",
                reason: format!("a {kind} carries no units"),
            });
        }
        if !variants.is_empty() && kind != ValueKind::Enum {
            return Err(TypeError::Invalid {
                field: "variants",
                reason: format!("a {kind} carries no variants"),
            });
        }
        if kind == ValueKind::Enum && variants.is_empty() {
            return Err(TypeError::MissingField {
                field: "variants",
                context: "an enum value type",
            });
        }
        unique("units", &units)?;
        unique("variants", &variants)?;
        Ok(Self {
            kind,
            units,
            variants,
        })
    }

    /// The value kind.
    #[must_use]
    pub const fn kind(&self) -> ValueKind {
        self.kind
    }

    /// The admitted units of a decimal.
    #[must_use]
    pub fn units(&self) -> &[Unit] {
        &self.units
    }

    /// The variants of an enumeration.
    #[must_use]
    pub fn variants(&self) -> &[EnumVariant] {
        &self.variants
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawValueType {
    #[serde(rename = "type")]
    kind: ValueKind,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    units: Vec<Unit>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    variants: Vec<EnumVariant>,
}

impl From<ValueType> for RawValueType {
    fn from(value: ValueType) -> Self {
        Self {
            kind: value.kind,
            units: value.units,
            variants: value.variants,
        }
    }
}

impl TryFrom<RawValueType> for ValueType {
    type Error = TypeError;

    fn try_from(raw: RawValueType) -> Result<Self> {
        Self::build(raw.kind, raw.units, raw.variants)
    }
}

fn unique<T: Ord>(field: &'static str, items: &[T]) -> Result<()> {
    let mut seen = std::collections::BTreeSet::new();
    if items.iter().all(|item| seen.insert(item)) {
        Ok(())
    } else {
        Err(TypeError::Invalid {
            field,
            reason: "lists an entry twice".to_owned(),
        })
    }
}

closed_vocabulary! {
    /// How many values a predicate holds for one subject (B-8).
    CardinalityKind, "cardinality", CARDINALITIES {
        /// One value per subject.
        One => "one",
        /// One value per slot key (B-4).
        Many => "many",
    }
}

/// How many values a predicate holds for one subject (B-4, B-8).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Cardinality {
    /// One value.
    One,
    /// One value per key named by the slot.
    Many(SlotName),
}

impl Cardinality {
    /// Which of the two this is.
    #[must_use]
    pub const fn kind(&self) -> CardinalityKind {
        match self {
            Self::One => CardinalityKind::One,
            Self::Many(_) => CardinalityKind::Many,
        }
    }
}

closed_vocabulary! {
    /// Where a claim's valid time comes from (052 B-2).
    ValidTimeMode, "valid_time", VALID_TIME_MODES {
        /// The claim states its own validity.
        Explicit => "explicit",
        /// Valid from the source's time, until superseded.
        FromSourceTime => "from_source_time",
        /// Always valid: an identifier such as a record locator.
        Timeless => "timeless",
    }
}

closed_vocabulary! {
    /// How claims in one slot supersede each other (052 B-5).
    SupersessionRule, "supersession", SUPERSESSION_RULES {
        /// A later source supersedes an earlier one over their overlap.
        BySourceOrder => "by_source_order",
        /// Only an explicit `Supersedes` relation supersedes.
        ExplicitOnly => "explicit_only",
        /// Values coexist; cardinality `many` without replacement.
        Accumulate => "accumulate",
    }
}

/// One predicate in a registry document (B-8).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "RawPredicateDef", into = "RawPredicateDef")]
pub struct PredicateDef {
    name: PredicateName,
    subjects: Vec<SubjectKind>,
    value: ValueType,
    cardinality: Cardinality,
    valid_time: ValidTimeMode,
    supersession: SupersessionRule,
    epistemic: Vec<Stance>,
    deprecated: bool,
}

/// The fields of a [`PredicateDef`], checked by [`PredicateDef::new`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PredicateDefParts {
    /// The name within the namespace.
    pub name: PredicateName,
    /// The subject kinds it applies to; at least one.
    pub subjects: Vec<SubjectKind>,
    /// The value type.
    pub value: ValueType,
    /// One value, or one per slot.
    pub cardinality: Cardinality,
    /// Where valid time comes from.
    pub valid_time: ValidTimeMode,
    /// How values in a slot supersede each other.
    pub supersession: SupersessionRule,
    /// The stances a claim of this predicate may take; at least one.
    pub epistemic: Vec<Stance>,
    /// Whether this version withdraws it from new claims (B-9).
    pub deprecated: bool,
}

impl PredicateDef {
    /// Check and build a predicate definition.
    ///
    /// # Errors
    ///
    /// [`TypeError`] naming the field when `subjects` or `epistemic` is
    /// empty or repeats an entry, or when `accumulate` is declared for a
    /// cardinality of one.
    pub fn new(parts: PredicateDefParts) -> Result<Self> {
        if parts.subjects.is_empty() {
            return Err(TypeError::MissingField {
                field: "subjects",
                context: "a predicate",
            });
        }
        if parts.epistemic.is_empty() {
            return Err(TypeError::MissingField {
                field: "epistemic",
                context: "a predicate",
            });
        }
        unique("subjects", &parts.subjects)?;
        unique("epistemic", &parts.epistemic)?;
        if parts.supersession == SupersessionRule::Accumulate
            && parts.cardinality == Cardinality::One
        {
            return Err(TypeError::Invalid {
                field: "supersession",
                reason: format!(
                    "{} accumulates values, which needs cardinality many",
                    parts.name
                ),
            });
        }
        Ok(Self {
            name: parts.name,
            subjects: parts.subjects,
            value: parts.value,
            cardinality: parts.cardinality,
            valid_time: parts.valid_time,
            supersession: parts.supersession,
            epistemic: parts.epistemic,
            deprecated: parts.deprecated,
        })
    }

    /// The name.
    #[must_use]
    pub const fn name(&self) -> &PredicateName {
        &self.name
    }

    /// The subject kinds it applies to.
    #[must_use]
    pub fn subjects(&self) -> &[SubjectKind] {
        &self.subjects
    }

    /// The value type.
    #[must_use]
    pub const fn value(&self) -> &ValueType {
        &self.value
    }

    /// The cardinality.
    #[must_use]
    pub const fn cardinality(&self) -> &Cardinality {
        &self.cardinality
    }

    /// The valid-time mode.
    #[must_use]
    pub const fn valid_time(&self) -> ValidTimeMode {
        self.valid_time
    }

    /// The supersession rule.
    #[must_use]
    pub const fn supersession(&self) -> SupersessionRule {
        self.supersession
    }

    /// The admitted stances.
    #[must_use]
    pub fn epistemic(&self) -> &[Stance] {
        &self.epistemic
    }

    /// Whether this version withdraws the predicate from new claims.
    #[must_use]
    pub const fn deprecated(&self) -> bool {
        self.deprecated
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPredicateDef {
    name: PredicateName,
    subjects: Vec<SubjectKind>,
    value: ValueType,
    cardinality: CardinalityKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    slot: Option<SlotName>,
    valid_time: ValidTimeMode,
    supersession: SupersessionRule,
    epistemic: Vec<Stance>,
    #[serde(default, skip_serializing_if = "core::ops::Not::not")]
    deprecated: bool,
}

impl From<PredicateDef> for RawPredicateDef {
    fn from(def: PredicateDef) -> Self {
        let cardinality = def.cardinality.kind();
        let slot = match def.cardinality {
            Cardinality::One => None,
            Cardinality::Many(slot) => Some(slot),
        };
        Self {
            name: def.name,
            subjects: def.subjects,
            value: def.value,
            cardinality,
            slot,
            valid_time: def.valid_time,
            supersession: def.supersession,
            epistemic: def.epistemic,
            deprecated: def.deprecated,
        }
    }
}

impl TryFrom<RawPredicateDef> for PredicateDef {
    type Error = TypeError;

    fn try_from(raw: RawPredicateDef) -> Result<Self> {
        let cardinality = match (raw.cardinality, raw.slot) {
            (CardinalityKind::One, None) => Cardinality::One,
            (CardinalityKind::Many, Some(slot)) => Cardinality::Many(slot),
            (CardinalityKind::One, Some(_)) => {
                return Err(TypeError::Invalid {
                    field: "slot",
                    reason: format!("{} has cardinality one and names a slot", raw.name),
                });
            }
            (CardinalityKind::Many, None) => {
                return Err(TypeError::MissingField {
                    field: "slot",
                    context: "cardinality many",
                });
            }
        };
        Self::new(PredicateDefParts {
            name: raw.name,
            subjects: raw.subjects,
            value: raw.value,
            cardinality,
            valid_time: raw.valid_time,
            supersession: raw.supersession,
            epistemic: raw.epistemic,
            deprecated: raw.deprecated,
        })
    }
}

/// One registered version of one namespace's vocabulary (B-8, B-9).
///
/// Immutable once registered (I-3). A later version is a new document.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "RawPredicateSet", into = "RawPredicateSet")]
pub struct PredicateSet {
    namespace: Namespace,
    version: u32,
    subject_kinds: Vec<SubjectKind>,
    predicates: Vec<PredicateDef>,
}

impl PredicateSet {
    /// Check and build a registry document.
    ///
    /// # Errors
    ///
    /// [`TypeError`] naming the field when `version` is zero, a subject kind
    /// or predicate name repeats, or a predicate applies to a subject kind
    /// the document does not declare.
    pub fn new(
        namespace: Namespace,
        version: u32,
        subject_kinds: Vec<SubjectKind>,
        predicates: Vec<PredicateDef>,
    ) -> Result<Self> {
        if version == 0 {
            return Err(TypeError::Invalid {
                field: "version",
                reason: "versions start at 1".to_owned(),
            });
        }
        unique("subject_kinds", &subject_kinds)?;
        let names: Vec<&PredicateName> = predicates.iter().map(PredicateDef::name).collect();
        unique("predicates", &names)?;
        for def in &predicates {
            if let Some(kind) = def
                .subjects
                .iter()
                .find(|kind| !subject_kinds.contains(kind))
            {
                return Err(TypeError::Invalid {
                    field: "subjects",
                    reason: format!(
                        "{} applies to {kind}, which the document does not declare",
                        def.name
                    ),
                });
            }
        }
        Ok(Self {
            namespace,
            version,
            subject_kinds,
            predicates,
        })
    }

    /// The namespace.
    #[must_use]
    pub const fn namespace(&self) -> &Namespace {
        &self.namespace
    }

    /// The version.
    #[must_use]
    pub const fn version(&self) -> u32 {
        self.version
    }

    /// The declared subject kinds.
    #[must_use]
    pub fn subject_kinds(&self) -> &[SubjectKind] {
        &self.subject_kinds
    }

    /// The predicates.
    #[must_use]
    pub fn predicates(&self) -> &[PredicateDef] {
        &self.predicates
    }

    /// The predicate named `name`, when this version declares it.
    #[must_use]
    pub fn predicate(&self, name: &PredicateName) -> Option<&PredicateDef> {
        self.predicates.iter().find(|def| &def.name == name)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPredicateSet {
    namespace: Namespace,
    version: u32,
    subject_kinds: Vec<SubjectKind>,
    predicates: Vec<PredicateDef>,
}

impl From<PredicateSet> for RawPredicateSet {
    fn from(set: PredicateSet) -> Self {
        Self {
            namespace: set.namespace,
            version: set.version,
            subject_kinds: set.subject_kinds,
            predicates: set.predicates,
        }
    }
}

impl TryFrom<RawPredicateSet> for PredicateSet {
    type Error = TypeError;

    fn try_from(raw: RawPredicateSet) -> Result<Self> {
        Self::new(
            raw.namespace,
            raw.version,
            raw.subject_kinds,
            raw.predicates,
        )
    }
}
