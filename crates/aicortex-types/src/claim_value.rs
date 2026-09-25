//! What a claim says, as a typed value (spec 050 B-5).
//!
//! A closed set of value types, each of which compares and hashes the same
//! on every target. There is no floating-point value: a fare is a
//! fixed-point [`Decimal`] with its currency, for the reason 013 D-4 gives
//! for the gate's arithmetic (050 I-4). An unknown value type is a
//! [`TypeError`] naming `type`, never a passthrough.

use core::fmt;

use serde::{Deserialize, Serialize};

use crate::claim::SubjectRef;
use crate::claim_time::{
    CivilDate, Interval, ZonedTime, checked_string, closed_vocabulary, lower_ident,
};
use crate::error::{Result, TypeError};

checked_string! {
    /// An ISO 4217 currency code: three uppercase ascii letters.
    CurrencyCode, "unit", currency_code
}

fn currency_code(field: &'static str, text: &str) -> Result<()> {
    if text.len() == 3 && text.bytes().all(|byte| byte.is_ascii_uppercase()) {
        Ok(())
    } else {
        Err(TypeError::Invalid {
            field,
            reason: format!("{text:?} is not an ISO 4217 code"),
        })
    }
}

checked_string! {
    /// A registered measurement unit, such as `km` or `night`.
    UnitName, "unit", lower_ident
}

closed_vocabulary! {
    /// The two kinds of unit a decimal may carry.
    UnitKind, "unit", UNIT_KINDS {
        /// An ISO 4217 currency.
        Currency => "currency",
        /// A measurement unit a registry admits.
        Measure => "measure",
    }
}

/// The unit of a [`Decimal`] (B-5).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "RawUnit", into = "RawUnit")]
pub enum Unit {
    /// A currency.
    Currency(CurrencyCode),
    /// A measurement unit.
    Measure(UnitName),
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawUnit {
    kind: UnitKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    code: Option<CurrencyCode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<UnitName>,
}

impl From<Unit> for RawUnit {
    fn from(unit: Unit) -> Self {
        match unit {
            Unit::Currency(code) => Self {
                kind: UnitKind::Currency,
                code: Some(code),
                name: None,
            },
            Unit::Measure(name) => Self {
                kind: UnitKind::Measure,
                code: None,
                name: Some(name),
            },
        }
    }
}

impl TryFrom<RawUnit> for Unit {
    type Error = TypeError;

    fn try_from(raw: RawUnit) -> Result<Self> {
        match (raw.kind, raw.code, raw.name) {
            (UnitKind::Currency, Some(code), None) => Ok(Self::Currency(code)),
            (UnitKind::Measure, None, Some(name)) => Ok(Self::Measure(name)),
            (kind, ..) => Err(TypeError::Invalid {
                field: "unit",
                reason: format!(
                    "a {kind} unit carries `{}` and nothing else",
                    if kind == UnitKind::Currency {
                        "code"
                    } else {
                        "name"
                    }
                ),
            }),
        }
    }
}

impl fmt::Display for Unit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Currency(code) => write!(f, "{code}"),
            Self::Measure(name) => write!(f, "{name}"),
        }
    }
}

/// The most digits after the point a decimal may carry: what fits an
/// `i128` mantissa.
pub const MAX_SCALE: u8 = 38;

/// A fixed-point number, `mantissa * 10^-scale`, with an optional unit
/// (B-5, FR-006).
///
/// Written as its exact decimal text (`"1234.50"`) so it round-trips byte
/// for byte and never passes through a binary floating-point type. `1.5`
/// and `1.50` are different values: the scale a source gave is kept.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "RawDecimal", into = "RawDecimal")]
pub struct Decimal {
    mantissa: i128,
    scale: u8,
    unit: Option<Unit>,
}

impl Decimal {
    /// `mantissa * 10^-scale`, in `unit`.
    ///
    /// # Errors
    ///
    /// [`TypeError::Invalid`] naming `amount` for a scale above
    /// [`MAX_SCALE`].
    pub fn new(mantissa: i128, scale: u8, unit: Option<Unit>) -> Result<Self> {
        if scale > MAX_SCALE {
            return Err(TypeError::Invalid {
                field: "amount",
                reason: format!("a scale of {scale} is above {MAX_SCALE}"),
            });
        }
        Ok(Self {
            mantissa,
            scale,
            unit,
        })
    }

    /// Parse the exact decimal text: an optional `-`, digits with no
    /// leading zero, and an optional point with digits after it.
    ///
    /// # Errors
    ///
    /// [`TypeError::Invalid`] naming `amount` for any other text, a negative
    /// zero, or a value beyond an `i128` mantissa.
    pub fn parse(text: &str, unit: Option<Unit>) -> Result<Self> {
        let invalid = |reason: &str| TypeError::Invalid {
            field: "amount",
            reason: format!("{text:?} {reason}"),
        };
        let (negative, magnitude) = match text.strip_prefix('-') {
            Some(rest) => (true, rest),
            None => (false, text),
        };
        let (whole, fraction) = magnitude.split_once('.').unwrap_or((magnitude, ""));
        let all_digits = |part: &str| part.bytes().all(|byte| byte.is_ascii_digit());
        if whole.is_empty() || !all_digits(whole) || !all_digits(fraction) {
            return Err(invalid("is not a decimal number"));
        }
        if magnitude.contains('.') && fraction.is_empty() {
            return Err(invalid("ends in a point"));
        }
        if whole.len() > 1 && whole.starts_with('0') {
            return Err(invalid("has a leading zero"));
        }
        let scale = u8::try_from(fraction.len()).map_err(|_| invalid("has too many digits"))?;
        let digits = format!("{whole}{fraction}");
        let magnitude: i128 = digits
            .parse()
            .map_err(|_| invalid("is beyond an i128 mantissa"))?;
        if negative && magnitude == 0 {
            return Err(invalid("is a negative zero"));
        }
        let mantissa = if negative { -magnitude } else { magnitude };
        Self::new(mantissa, scale, unit)
    }

    /// The mantissa.
    #[must_use]
    pub const fn mantissa(&self) -> i128 {
        self.mantissa
    }

    /// Digits after the point.
    #[must_use]
    pub const fn scale(&self) -> u8 {
        self.scale
    }

    /// The unit, when there is one.
    #[must_use]
    pub const fn unit(&self) -> Option<&Unit> {
        self.unit.as_ref()
    }

    /// The exact decimal text, without the unit.
    #[must_use]
    pub fn amount(&self) -> String {
        let digits = self.mantissa.unsigned_abs().to_string();
        let scale = usize::from(self.scale);
        let padded = format!("{digits:0>width$}", width = scale + 1);
        let split = padded.len() - scale;
        let (whole, fraction) = padded.split_at(split);
        let sign = if self.mantissa < 0 { "-" } else { "" };
        if fraction.is_empty() {
            format!("{sign}{whole}")
        } else {
            format!("{sign}{whole}.{fraction}")
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDecimal {
    amount: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    unit: Option<Unit>,
}

impl From<Decimal> for RawDecimal {
    fn from(decimal: Decimal) -> Self {
        Self {
            amount: decimal.amount(),
            unit: decimal.unit,
        }
    }
}

impl TryFrom<RawDecimal> for Decimal {
    type Error = TypeError;

    fn try_from(raw: RawDecimal) -> Result<Self> {
        Self::parse(&raw.amount, raw.unit)
    }
}

checked_string! {
    /// A variant of an enumeration a predicate declares, such as `ticketed`.
    EnumVariant, "variant", lower_ident
}

closed_vocabulary! {
    /// The value types a claim may carry (B-5), and a predicate may declare
    /// (B-8).
    ValueKind, "type", VALUE_KINDS {
        /// [`ClaimValue::Text`].
        Text => "text",
        /// [`ClaimValue::Integer`].
        Integer => "integer",
        /// [`ClaimValue::Decimal`].
        Decimal => "decimal",
        /// [`ClaimValue::Date`].
        Date => "date",
        /// [`ClaimValue::DateTime`].
        DateTime => "datetime",
        /// [`ClaimValue::Interval`].
        Interval => "interval",
        /// [`ClaimValue::EntityRef`].
        EntityRef => "entity_ref",
        /// [`ClaimValue::Enum`].
        Enum => "enum",
    }
}

/// What a claim says (B-5). Closed.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "RawValue", into = "RawValue")]
pub enum ClaimValue {
    /// Text, normalized as the gate normalizes (013 B-8) before admission.
    Text(String),
    /// A whole number.
    Integer(i64),
    /// A fixed-point number with an optional unit or currency.
    Decimal(Decimal),
    /// A date at year, month or day precision.
    Date(CivilDate),
    /// A date and time with its zone form.
    DateTime(ZonedTime),
    /// Two bounds of dates or date-times.
    Interval(Interval),
    /// Another subject.
    EntityRef(SubjectRef),
    /// A variant of the predicate's enumeration.
    Enum(EnumVariant),
}

impl ClaimValue {
    /// The value's type.
    #[must_use]
    pub const fn kind(&self) -> ValueKind {
        match self {
            Self::Text(_) => ValueKind::Text,
            Self::Integer(_) => ValueKind::Integer,
            Self::Decimal(_) => ValueKind::Decimal,
            Self::Date(_) => ValueKind::Date,
            Self::DateTime(_) => ValueKind::DateTime,
            Self::Interval(_) => ValueKind::Interval,
            Self::EntityRef(_) => ValueKind::EntityRef,
            Self::Enum(_) => ValueKind::Enum,
        }
    }
}

/// The wire form: `type` names the value type and exactly the one field of
/// that name carries it.
#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawValue {
    #[serde(rename = "type")]
    kind: Option<ValueKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    integer: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    decimal: Option<Decimal>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    date: Option<CivilDate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    datetime: Option<ZonedTime>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    interval: Option<Interval>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    entity_ref: Option<SubjectRef>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "enum")]
    variant: Option<EnumVariant>,
}

impl RawValue {
    const fn carried(&self) -> usize {
        self.text.is_some() as usize
            + self.integer.is_some() as usize
            + self.decimal.is_some() as usize
            + self.date.is_some() as usize
            + self.datetime.is_some() as usize
            + self.interval.is_some() as usize
            + self.entity_ref.is_some() as usize
            + self.variant.is_some() as usize
    }
}

impl From<ClaimValue> for RawValue {
    fn from(value: ClaimValue) -> Self {
        let kind = Some(value.kind());
        match value {
            ClaimValue::Text(text) => Self {
                kind,
                text: Some(text),
                ..Self::default()
            },
            ClaimValue::Integer(integer) => Self {
                kind,
                integer: Some(integer),
                ..Self::default()
            },
            ClaimValue::Decimal(decimal) => Self {
                kind,
                decimal: Some(decimal),
                ..Self::default()
            },
            ClaimValue::Date(date) => Self {
                kind,
                date: Some(date),
                ..Self::default()
            },
            ClaimValue::DateTime(time) => Self {
                kind,
                datetime: Some(time),
                ..Self::default()
            },
            ClaimValue::Interval(interval) => Self {
                kind,
                interval: Some(interval),
                ..Self::default()
            },
            ClaimValue::EntityRef(subject) => Self {
                kind,
                entity_ref: Some(subject),
                ..Self::default()
            },
            ClaimValue::Enum(variant) => Self {
                kind,
                variant: Some(variant),
                ..Self::default()
            },
        }
    }
}

impl TryFrom<RawValue> for ClaimValue {
    type Error = TypeError;

    fn try_from(raw: RawValue) -> Result<Self> {
        let kind = raw.kind.ok_or(TypeError::MissingField {
            field: "type",
            context: "a claim value",
        })?;
        let mismatch = || TypeError::Invalid {
            field: "type",
            reason: format!("a {kind} value carries the `{kind}` field and no other"),
        };
        if raw.carried() != 1 {
            return Err(mismatch());
        }
        let value = match kind {
            ValueKind::Text => raw.text.map(Self::Text),
            ValueKind::Integer => raw.integer.map(Self::Integer),
            ValueKind::Decimal => raw.decimal.map(Self::Decimal),
            ValueKind::Date => raw.date.map(Self::Date),
            ValueKind::DateTime => raw.datetime.map(Self::DateTime),
            ValueKind::Interval => raw.interval.map(Self::Interval),
            ValueKind::EntityRef => raw.entity_ref.map(Self::EntityRef),
            ValueKind::Enum => raw.variant.map(Self::Enum),
        };
        value.ok_or_else(mismatch)
    }
}
