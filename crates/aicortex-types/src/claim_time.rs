//! Time as a claim states it (spec 050 B-6, B-7).
//!
//! A source says "3 October", "14:05 in Warsaw", "between 14:00 and 16:00",
//! or a boarding-pass time with no zone at all. Each of those is kept in the
//! form it was given: a date keeps its precision, so a day is not silently
//! read as midnight; a time keeps its zone form, so a floating wall-clock
//! time is not converted with the host's zone. Turning a value into an
//! instant is a function of the value and a pinned time-zone database
//! version, which is spec 052's projection policy and not anything here.
//!
//! These types are shared by the claim values of `claim_value` and by 052's
//! valid time. They carry no clock and no zone database.
//!
//! The closed vocabularies of the claim modules (precision, zone form, value
//! type, stance, relation kind, ...) are declared with [`closed_vocabulary`],
//! so each refuses a word outside it with a [`TypeError`] naming its field,
//! as 011 FR-002 requires and 050 FR-002 repeats.

use core::fmt;
use core::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::error::{Result, TypeError, validate_key};

/// Declare a closed vocabulary: an enum of unit variants, its labels, and a
/// serde form that refuses any other word by naming `$field`.
macro_rules! closed_vocabulary {
    (
        $(#[$meta:meta])*
        $name:ident, $field:literal, $words:ident {
            $($(#[$vmeta:meta])* $variant:ident => $label:literal),+ $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub enum $name {
            $($(#[$vmeta])* $variant),+
        }

        /// The whole vocabulary, in declaration order, for refusal messages.
        const $words: &[&str] = &[$($label),+];

        impl $name {
            /// The label, as the wire and a registry document write it.
            #[must_use]
            pub const fn label(self) -> &'static str {
                match self {
                    $(Self::$variant => $label),+
                }
            }

            /// Every variant, in declaration order.
            #[must_use]
            pub const fn all() -> &'static [Self] {
                &[$(Self::$variant),+]
            }
        }

        impl core::str::FromStr for $name {
            type Err = crate::error::TypeError;

            fn from_str(text: &str) -> crate::error::Result<Self> {
                match text {
                    $($label => Ok(Self::$variant),)+
                    other => Err(crate::error::unknown_variant($field, other, $words)),
                }
            }
        }

        impl core::fmt::Display for $name {
            fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                f.write_str(self.label())
            }
        }

        impl serde::Serialize for $name {
            fn serialize<S: serde::Serializer>(
                &self,
                serializer: S,
            ) -> core::result::Result<S::Ok, S::Error> {
                serializer.serialize_str(self.label())
            }
        }

        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(
                deserializer: D,
            ) -> core::result::Result<Self, D::Error> {
                let text = String::deserialize(deserializer)?;
                text.parse().map_err(serde::de::Error::custom)
            }
        }
    };
}

/// Declare a validated string: a newtype that exists only in a legal shape,
/// serialized as the bare string and checked again on the way in.
macro_rules! checked_string {
    (
        $(#[$meta:meta])*
        $name:ident, $field:literal, $check:path
    ) => {
        $(#[$meta])*
        #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Validate and wrap.
            ///
            /// # Errors
            ///
            /// [`crate::TypeError::Invalid`] naming the field when the text is
            /// not in this type's shape.
            pub fn new(text: impl Into<String>) -> crate::error::Result<Self> {
                let text = text.into();
                $check($field, &text)?;
                Ok(Self(text))
            }

            /// The text.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl core::fmt::Display for $name {
            fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(
                deserializer: D,
            ) -> core::result::Result<Self, D::Error> {
                let text = String::deserialize(deserializer)?;
                Self::new(text).map_err(serde::de::Error::custom)
            }
        }
    };
}

pub(crate) use {checked_string, closed_vocabulary};

/// A lowercase identifier: `a-z` first, then `a-z`, `0-9`, `_`, `-`.
///
/// The shape of a namespace, a subject kind, a slot name, an enumeration
/// variant, and a measurement unit: each reaches a registry document, a SQL
/// parameter and a path segment, so none may carry anything else.
pub(crate) fn lower_ident(field: &'static str, text: &str) -> Result<()> {
    validate_key(field, text)?;
    let mut bytes = text.bytes();
    let first_is_letter = bytes.next().is_some_and(|byte| byte.is_ascii_lowercase());
    let rest_legal = bytes.all(|byte| {
        byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_' || byte == b'-'
    });
    if first_is_letter && rest_legal {
        Ok(())
    } else {
        Err(TypeError::Invalid {
            field,
            reason: format!(
                "{text:?} is not a lowercase identifier (`a-z` first, then `a-z`, `0-9`, `_`, `-`)"
            ),
        })
    }
}

closed_vocabulary! {
    /// How finely a time point was stated (B-7).
    Precision, "precision", PRECISIONS {
        /// A year: "2026".
        Year => "year",
        /// A month: "2026-10".
        Month => "month",
        /// A day: "2026-10-03".
        Day => "day",
        /// An hour of a day: "2026-10-03T14".
        Hour => "hour",
        /// A minute: "2026-10-03T14:05".
        Minute => "minute",
        /// A second: "2026-10-03T14:05:30". Nothing finer is kept.
        Second => "second",
    }
}

const fn is_leap(year: u16) -> bool {
    year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400))
}

const fn days_in(year: u16, month: u8) -> u8 {
    match month {
        2 if is_leap(year) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Parse exactly `width` ascii digits.
fn digits(field: &'static str, text: &str, width: usize) -> Result<u16> {
    if text.len() != width || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(TypeError::Invalid {
            field,
            reason: format!("{text:?} is not {width} digits"),
        });
    }
    text.parse().map_err(|_| TypeError::Invalid {
        field,
        reason: format!("{text:?} is not a number"),
    })
}

/// A calendar date at year, month or day precision, with no time and no
/// zone (B-5, B-7).
///
/// Written in the reduced ISO 8601 form its precision implies ("2026",
/// "2026-10", "2026-10-03") with the precision beside it, so a reader never
/// infers a day the source did not state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CivilDate {
    year: u16,
    month: Option<u8>,
    day: Option<u8>,
}

impl CivilDate {
    /// A day.
    ///
    /// # Errors
    ///
    /// [`TypeError::Invalid`] naming `date` when the year is outside
    /// 1 to 9999 or the month or day does not exist.
    pub fn ymd(year: u16, month: u8, day: u8) -> Result<Self> {
        let date = Self::ym(year, month)?;
        if day == 0 || day > days_in(year, month) {
            return Err(TypeError::Invalid {
                field: "date",
                reason: format!("{year:04}-{month:02} has no day {day}"),
            });
        }
        Ok(Self {
            day: Some(day),
            ..date
        })
    }

    /// A month.
    ///
    /// # Errors
    ///
    /// [`TypeError::Invalid`] naming `date` for a year outside 1 to 9999 or
    /// a month outside 1 to 12.
    pub fn ym(year: u16, month: u8) -> Result<Self> {
        let date = Self::y(year)?;
        if !(1..=12).contains(&month) {
            return Err(TypeError::Invalid {
                field: "date",
                reason: format!("{month} is not a month"),
            });
        }
        Ok(Self {
            month: Some(month),
            ..date
        })
    }

    /// A year.
    ///
    /// # Errors
    ///
    /// [`TypeError::Invalid`] naming `date` for a year outside 1 to 9999.
    pub fn y(year: u16) -> Result<Self> {
        if !(1..=9999).contains(&year) {
            return Err(TypeError::Invalid {
                field: "date",
                reason: format!("{year} is outside 1 to 9999"),
            });
        }
        Ok(Self {
            year,
            month: None,
            day: None,
        })
    }

    /// How finely the date was stated.
    #[must_use]
    pub const fn precision(&self) -> Precision {
        match (self.month, self.day) {
            (_, Some(_)) => Precision::Day,
            (Some(_), None) => Precision::Month,
            (None, None) => Precision::Year,
        }
    }

    /// The year.
    #[must_use]
    pub const fn year(&self) -> u16 {
        self.year
    }

    /// The month, when stated.
    #[must_use]
    pub const fn month(&self) -> Option<u8> {
        self.month
    }

    /// The day, when stated.
    #[must_use]
    pub const fn day(&self) -> Option<u8> {
        self.day
    }

    fn parse(text: &str) -> Result<Self> {
        let mut parts = text.split('-');
        let year = digits("date", parts.next().unwrap_or_default(), 4)?;
        let month = parts
            .next()
            .map(|part| digits("date", part, 2))
            .transpose()?;
        let day = parts
            .next()
            .map(|part| digits("date", part, 2))
            .transpose()?;
        if parts.next().is_some() {
            return Err(TypeError::Invalid {
                field: "date",
                reason: format!("{text:?} is not YYYY, YYYY-MM or YYYY-MM-DD"),
            });
        }
        let narrow = |value: u16| u8::try_from(value).unwrap_or(u8::MAX);
        match (month, day) {
            (None, _) => Self::y(year),
            (Some(month), None) => Self::ym(year, narrow(month)),
            (Some(month), Some(day)) => Self::ymd(year, narrow(month), narrow(day)),
        }
    }
}

impl fmt::Display for CivilDate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:04}", self.year)?;
        if let Some(month) = self.month {
            write!(f, "-{month:02}")?;
        }
        if let Some(day) = self.day {
            write!(f, "-{day:02}")?;
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDate {
    on: String,
    precision: Precision,
}

impl Serialize for CivilDate {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> core::result::Result<S::Ok, S::Error> {
        RawDate {
            on: self.to_string(),
            precision: self.precision(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for CivilDate {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> core::result::Result<Self, D::Error> {
        let raw = RawDate::deserialize(deserializer)?;
        let date = Self::parse(&raw.on).map_err(serde::de::Error::custom)?;
        agree("precision", date.precision(), raw.precision).map_err(serde::de::Error::custom)?;
        Ok(date)
    }
}

fn agree(field: &'static str, stated: Precision, declared: Precision) -> Result<()> {
    if stated == declared {
        Ok(())
    } else {
        Err(TypeError::Invalid {
            field,
            reason: format!("declared {declared} but the value is written at {stated}"),
        })
    }
}

checked_string! {
    /// An IANA time-zone name, such as `Europe/Warsaw` (B-6).
    ///
    /// Held to a shape, not checked against a database: which names exist is
    /// a fact of the pinned database version 052's projection names, and a
    /// value must not change meaning when that database is upgraded.
    TzName, "zone", tz_name
}

fn tz_name(field: &'static str, text: &str) -> Result<()> {
    validate_key(field, text)?;
    let legal = text
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || b"/_-+".contains(&byte));
    if !legal || text.starts_with('/') || text.ends_with('/') || text.contains("//") {
        return Err(TypeError::Invalid {
            field,
            reason: format!("{text:?} is not shaped like an IANA zone name"),
        });
    }
    Ok(())
}

closed_vocabulary! {
    /// The three zone forms a source can give (B-6).
    ZoneForm, "zone", ZONE_FORMS {
        /// A named IANA zone.
        Iana => "iana",
        /// A fixed offset from UTC.
        Offset => "offset",
        /// No zone at all: a wall-clock time as printed.
        Floating => "floating",
    }
}

/// The largest offset a zone may carry, in seconds: eighteen hours.
const MAX_OFFSET_SECONDS: i32 = 18 * 3600;

/// The zone a time was given in, kept in the form the source gave (B-6).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "RawZone", into = "RawZone")]
pub enum Zone {
    /// A named zone.
    Iana(TzName),
    /// A fixed offset from UTC, in seconds east.
    Offset(i32),
    /// No zone: the wall-clock time as the source printed it.
    Floating,
}

impl Zone {
    /// A fixed offset.
    ///
    /// # Errors
    ///
    /// [`TypeError::Invalid`] naming `zone` for an offset beyond eighteen
    /// hours.
    pub fn offset(seconds: i32) -> Result<Self> {
        if seconds.unsigned_abs() > MAX_OFFSET_SECONDS.unsigned_abs() {
            return Err(TypeError::Invalid {
                field: "zone",
                reason: format!("{seconds} seconds is beyond eighteen hours"),
            });
        }
        Ok(Self::Offset(seconds))
    }

    /// Which of the three forms this is.
    #[must_use]
    pub const fn form(&self) -> ZoneForm {
        match self {
            Self::Iana(_) => ZoneForm::Iana,
            Self::Offset(_) => ZoneForm::Offset,
            Self::Floating => ZoneForm::Floating,
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawZone {
    kind: ZoneForm,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<TzName>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    seconds: Option<i32>,
}

impl From<Zone> for RawZone {
    fn from(zone: Zone) -> Self {
        let kind = zone.form();
        match zone {
            Zone::Iana(name) => Self {
                kind,
                name: Some(name),
                seconds: None,
            },
            Zone::Offset(seconds) => Self {
                kind,
                name: None,
                seconds: Some(seconds),
            },
            Zone::Floating => Self {
                kind,
                name: None,
                seconds: None,
            },
        }
    }
}

impl TryFrom<RawZone> for Zone {
    type Error = TypeError;

    fn try_from(raw: RawZone) -> Result<Self> {
        match (raw.kind, raw.name, raw.seconds) {
            (ZoneForm::Iana, Some(name), None) => Ok(Self::Iana(name)),
            (ZoneForm::Offset, None, Some(seconds)) => Self::offset(seconds),
            (ZoneForm::Floating, None, None) => Ok(Self::Floating),
            (kind, ..) => Err(TypeError::Invalid {
                field: "zone",
                reason: format!(
                    "a {kind} zone carries {}",
                    match kind {
                        ZoneForm::Iana => "`name` and nothing else",
                        ZoneForm::Offset => "`seconds` and nothing else",
                        ZoneForm::Floating => "no other field",
                    }
                ),
            }),
        }
    }
}

/// A civil date and time, at hour, minute or second precision, in the zone
/// form the source gave (B-5, B-6).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ZonedTime {
    date: CivilDate,
    hour: u8,
    minute: Option<u8>,
    second: Option<u8>,
    zone: Zone,
}

impl ZonedTime {
    /// A time at minute precision, the common case of a timetable.
    ///
    /// # Errors
    ///
    /// [`TypeError::Invalid`] naming `local` when `date` is not at day
    /// precision or the hour or minute is out of range.
    pub fn minute(date: CivilDate, hour: u8, minute: u8, zone: Zone) -> Result<Self> {
        Self::build(date, hour, Some(minute), None, zone)
    }

    /// A time at second precision.
    ///
    /// # Errors
    ///
    /// As [`Self::minute`], and for a second outside 0 to 59.
    pub fn second(date: CivilDate, hour: u8, minute: u8, second: u8, zone: Zone) -> Result<Self> {
        Self::build(date, hour, Some(minute), Some(second), zone)
    }

    /// A time at hour precision.
    ///
    /// # Errors
    ///
    /// As [`Self::minute`].
    pub fn hour(date: CivilDate, hour: u8, zone: Zone) -> Result<Self> {
        Self::build(date, hour, None, None, zone)
    }

    fn build(
        date: CivilDate,
        hour: u8,
        minute: Option<u8>,
        second: Option<u8>,
        zone: Zone,
    ) -> Result<Self> {
        let invalid = |reason: String| TypeError::Invalid {
            field: "local",
            reason,
        };
        if date.precision() != Precision::Day {
            return Err(invalid(format!("{date} is not a day")));
        }
        if hour > 23 {
            return Err(invalid(format!("{hour} is not an hour")));
        }
        if minute.is_some_and(|minute| minute > 59) || second.is_some_and(|second| second > 59) {
            return Err(invalid("a minute or second is outside 0 to 59".to_owned()));
        }
        if second.is_some() && minute.is_none() {
            return Err(invalid("a second without a minute".to_owned()));
        }
        Ok(Self {
            date,
            hour,
            minute,
            second,
            zone,
        })
    }

    /// How finely the time was stated.
    #[must_use]
    pub const fn precision(&self) -> Precision {
        match (self.minute, self.second) {
            (_, Some(_)) => Precision::Second,
            (Some(_), None) => Precision::Minute,
            (None, None) => Precision::Hour,
        }
    }

    /// The day.
    #[must_use]
    pub const fn date(&self) -> CivilDate {
        self.date
    }

    /// The stated wall-clock hour.
    #[must_use]
    pub const fn hour_value(&self) -> u8 {
        self.hour
    }

    /// The stated wall-clock minute, when precision includes it.
    #[must_use]
    pub const fn minute_value(&self) -> Option<u8> {
        self.minute
    }

    /// The stated wall-clock second, when precision includes it.
    #[must_use]
    pub const fn second_value(&self) -> Option<u8> {
        self.second
    }

    /// The zone form, as given.
    #[must_use]
    pub const fn zone(&self) -> &Zone {
        &self.zone
    }

    /// The wall-clock fields, for ordering two times in the same zone.
    const fn wall(&self) -> (CivilDate, u8, Option<u8>, Option<u8>) {
        (self.date, self.hour, self.minute, self.second)
    }

    fn local(&self) -> String {
        let mut text = format!("{}T{:02}", self.date, self.hour);
        if let Some(minute) = self.minute {
            text.push_str(&format!(":{minute:02}"));
        }
        if let Some(second) = self.second {
            text.push_str(&format!(":{second:02}"));
        }
        text
    }

    fn parse(local: &str, zone: Zone) -> Result<Self> {
        let invalid = || TypeError::Invalid {
            field: "local",
            reason: format!("{local:?} is not YYYY-MM-DDTHH, THH:MM or THH:MM:SS"),
        };
        let (date, time) = local.split_once('T').ok_or_else(invalid)?;
        let date = CivilDate::parse(date)?;
        let mut parts = time.split(':');
        let narrow = |value: u16| u8::try_from(value).unwrap_or(u8::MAX);
        let hour = narrow(digits("local", parts.next().unwrap_or_default(), 2)?);
        let minute = parts
            .next()
            .map(|part| digits("local", part, 2))
            .transpose()?;
        let second = parts
            .next()
            .map(|part| digits("local", part, 2))
            .transpose()?;
        if parts.next().is_some() {
            return Err(invalid());
        }
        Self::build(date, hour, minute.map(narrow), second.map(narrow), zone)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawZonedTime {
    local: String,
    precision: Precision,
    zone: Zone,
}

impl Serialize for ZonedTime {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> core::result::Result<S::Ok, S::Error> {
        RawZonedTime {
            local: self.local(),
            precision: self.precision(),
            zone: self.zone.clone(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ZonedTime {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> core::result::Result<Self, D::Error> {
        let raw = RawZonedTime::deserialize(deserializer)?;
        let time = Self::parse(&raw.local, raw.zone).map_err(serde::de::Error::custom)?;
        agree("precision", time.precision(), raw.precision).map_err(serde::de::Error::custom)?;
        Ok(time)
    }
}

/// An explicit window the source stated for a time point: "arrives between
/// 14:00 and 16:00" (B-7).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "RawUncertainty", into = "RawUncertainty")]
pub struct Uncertainty {
    earliest: ZonedTime,
    latest: ZonedTime,
}

impl Uncertainty {
    /// A window from `earliest` to `latest`.
    ///
    /// # Errors
    ///
    /// [`TypeError::Invalid`] naming `uncertainty` when the two are in the
    /// same zone and `latest` is before `earliest`. Times in different zone
    /// forms cannot be ordered without a zone database and are kept as given.
    pub fn new(earliest: ZonedTime, latest: ZonedTime) -> Result<Self> {
        if earliest.zone == latest.zone && latest.wall() < earliest.wall() {
            return Err(TypeError::Invalid {
                field: "uncertainty",
                reason: "latest is before earliest".to_owned(),
            });
        }
        Ok(Self { earliest, latest })
    }

    /// The earliest the point may be.
    #[must_use]
    pub const fn earliest(&self) -> &ZonedTime {
        &self.earliest
    }

    /// The latest the point may be.
    #[must_use]
    pub const fn latest(&self) -> &ZonedTime {
        &self.latest
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawUncertainty {
    earliest: ZonedTime,
    latest: ZonedTime,
}

impl From<Uncertainty> for RawUncertainty {
    fn from(window: Uncertainty) -> Self {
        Self {
            earliest: window.earliest,
            latest: window.latest,
        }
    }
}

impl TryFrom<RawUncertainty> for Uncertainty {
    type Error = TypeError;

    fn try_from(raw: RawUncertainty) -> Result<Self> {
        Self::new(raw.earliest, raw.latest)
    }
}

closed_vocabulary! {
    /// Whether a time point is a date or a date and time.
    PointKind, "point", POINT_KINDS {
        /// A [`CivilDate`].
        Date => "date",
        /// A [`ZonedTime`].
        DateTime => "datetime",
    }
}

/// A date or a date and time, as an interval bound or a valid-time bound
/// (B-7, 052 B-1).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TimeValue {
    /// A date at its own precision.
    Date(CivilDate),
    /// A date and time in its zone form.
    DateTime(ZonedTime),
}

impl TimeValue {
    /// Which of the two this is.
    #[must_use]
    pub const fn kind(&self) -> PointKind {
        match self {
            Self::Date(_) => PointKind::Date,
            Self::DateTime(_) => PointKind::DateTime,
        }
    }
}

/// A time point: a value, and optionally the window the source stated for it
/// (B-7).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "RawPoint", into = "RawPoint")]
pub struct TimePoint {
    value: TimeValue,
    uncertainty: Option<Uncertainty>,
}

impl TimePoint {
    /// A point with no stated window.
    #[must_use]
    pub const fn exact(value: TimeValue) -> Self {
        Self {
            value,
            uncertainty: None,
        }
    }

    /// A point with the window the source stated.
    #[must_use]
    pub const fn within(value: TimeValue, uncertainty: Uncertainty) -> Self {
        Self {
            value,
            uncertainty: Some(uncertainty),
        }
    }

    /// The value.
    #[must_use]
    pub const fn value(&self) -> &TimeValue {
        &self.value
    }

    /// The stated window, when there is one.
    #[must_use]
    pub const fn uncertainty(&self) -> Option<&Uncertainty> {
        self.uncertainty.as_ref()
    }

    /// Whether `self` is known to be after `other`, comparing only what can
    /// be compared without a zone database: two dates at the same precision,
    /// or two times in the same zone.
    fn known_after(&self, other: &Self) -> bool {
        match (&self.value, &other.value) {
            (TimeValue::Date(a), TimeValue::Date(b)) => a.precision() == b.precision() && a > b,
            (TimeValue::DateTime(a), TimeValue::DateTime(b)) => {
                a.zone == b.zone && a.wall() > b.wall()
            }
            _ => false,
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPoint {
    kind: PointKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    date: Option<CivilDate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    datetime: Option<ZonedTime>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    uncertainty: Option<Uncertainty>,
}

impl From<TimePoint> for RawPoint {
    fn from(point: TimePoint) -> Self {
        let kind = point.value.kind();
        let (date, datetime) = match point.value {
            TimeValue::Date(date) => (Some(date), None),
            TimeValue::DateTime(time) => (None, Some(time)),
        };
        Self {
            kind,
            date,
            datetime,
            uncertainty: point.uncertainty,
        }
    }
}

impl TryFrom<RawPoint> for TimePoint {
    type Error = TypeError;

    fn try_from(raw: RawPoint) -> Result<Self> {
        let value = match (raw.kind, raw.date, raw.datetime) {
            (PointKind::Date, Some(date), None) => TimeValue::Date(date),
            (PointKind::DateTime, None, Some(time)) => TimeValue::DateTime(time),
            (kind, ..) => {
                return Err(TypeError::Invalid {
                    field: "point",
                    reason: format!("a {kind} point carries `{kind}` and not the other"),
                });
            }
        };
        Ok(Self {
            value,
            uncertainty: raw.uncertainty,
        })
    }
}

closed_vocabulary! {
    /// How an interval bound holds (B-7).
    BoundKind, "bound", BOUND_KINDS {
        /// Unbounded on this side.
        Open => "open",
        /// The point is inside the interval.
        Inclusive => "inclusive",
        /// The point is outside the interval.
        Exclusive => "exclusive",
    }
}

/// One side of an interval (B-7).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "RawBound", into = "RawBound")]
pub enum Bound {
    /// Unbounded.
    Open,
    /// Includes the point.
    Inclusive(TimePoint),
    /// Excludes the point.
    Exclusive(TimePoint),
}

impl Bound {
    const fn point(&self) -> Option<&TimePoint> {
        match self {
            Self::Open => None,
            Self::Inclusive(point) | Self::Exclusive(point) => Some(point),
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawBound {
    kind: BoundKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    point: Option<TimePoint>,
}

impl From<Bound> for RawBound {
    fn from(bound: Bound) -> Self {
        match bound {
            Bound::Open => Self {
                kind: BoundKind::Open,
                point: None,
            },
            Bound::Inclusive(point) => Self {
                kind: BoundKind::Inclusive,
                point: Some(point),
            },
            Bound::Exclusive(point) => Self {
                kind: BoundKind::Exclusive,
                point: Some(point),
            },
        }
    }
}

impl TryFrom<RawBound> for Bound {
    type Error = TypeError;

    fn try_from(raw: RawBound) -> Result<Self> {
        match (raw.kind, raw.point) {
            (BoundKind::Open, None) => Ok(Self::Open),
            (BoundKind::Inclusive, Some(point)) => Ok(Self::Inclusive(point)),
            (BoundKind::Exclusive, Some(point)) => Ok(Self::Exclusive(point)),
            (kind, _) => Err(TypeError::Invalid {
                field: "bound",
                reason: format!(
                    "an {kind} bound {} a `point`",
                    if kind == BoundKind::Open {
                        "carries no"
                    } else {
                        "requires"
                    }
                ),
            }),
        }
    }
}

/// Two bounds of the same point kind, either of which may be open (B-7).
///
/// A value that is itself an interval, such as a hotel stay, is what the
/// claim says; it is not the claim's valid time (052 B-2).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "RawInterval", into = "RawInterval")]
pub struct Interval {
    start: Bound,
    end: Bound,
}

impl Interval {
    /// An interval from `start` to `end`.
    ///
    /// # Errors
    ///
    /// [`TypeError::Invalid`] naming `interval` when the two closed bounds
    /// are of different point kinds, or when they can be ordered and `end`
    /// is before `start`.
    pub fn new(start: Bound, end: Bound) -> Result<Self> {
        if let (Some(from), Some(to)) = (start.point(), end.point()) {
            if from.value.kind() != to.value.kind() {
                return Err(TypeError::Invalid {
                    field: "interval",
                    reason: "the bounds are a date and a date-time".to_owned(),
                });
            }
            if from.known_after(to) {
                return Err(TypeError::Invalid {
                    field: "interval",
                    reason: "the end is before the start".to_owned(),
                });
            }
        }
        Ok(Self { start, end })
    }

    /// The start bound.
    #[must_use]
    pub const fn start(&self) -> &Bound {
        &self.start
    }

    /// The end bound.
    #[must_use]
    pub const fn end(&self) -> &Bound {
        &self.end
    }

    /// The point kind of its closed bounds, or `None` when both are open.
    #[must_use]
    pub fn point_kind(&self) -> Option<PointKind> {
        self.start
            .point()
            .or_else(|| self.end.point())
            .map(|point| point.value.kind())
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawInterval {
    start: Bound,
    end: Bound,
}

impl From<Interval> for RawInterval {
    fn from(interval: Interval) -> Self {
        Self {
            start: interval.start,
            end: interval.end,
        }
    }
}

impl TryFrom<RawInterval> for Interval {
    type Error = TypeError;

    fn try_from(raw: RawInterval) -> Result<Self> {
        Self::new(raw.start, raw.end)
    }
}

impl FromStr for TzName {
    type Err = TypeError;

    fn from_str(text: &str) -> Result<Self> {
        Self::new(text)
    }
}
