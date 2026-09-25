//! The claim, checked (spec 050).
//!
//! - FR-001, AC-1: every fixture under `testdata/claims/` deserializes and
//!   re-serializes byte for byte. The 011 fixtures still round-trip with
//!   `spans` added to `Provenance`; `tests/model.rs` asserts that over
//!   `testdata/memories/`, unchanged.
//! - FR-002: an unknown value type, zone form, precision, stance or relation
//!   kind is a typed error naming its field.
//! - FR-003: the compile-fail cases under `tests/claim_compile_fail/`.
//! - FR-006: a decimal compares equal to itself across serialization and
//!   never passes through a floating-point type.
//!
//! `AICORTEX_FIXTURES=overwrite cargo test -p aicortex-types --test claim`
//! rewrites the fixtures from `fixtures()` and asserts nothing; every other
//! run reads them and compares.

use std::fs;
use std::path::PathBuf;

use aicortex_types::{
    Bound, CivilDate, Claim, ClaimId, ClaimParts, ClaimRelation, ClaimValue, ContentDigest,
    CurrencyCode, Decimal, EnumVariant, EpistemicStatus, Hex, Interval, MemoryId, Namespace,
    PartLocator, PredicateRef, Provenance, RelationKind, RelationTarget, Scope, SlotKey, SourceRef,
    SourceSpan, SourceSystem, SpanRange, SpanUnit, Stance, SubjectKey, SubjectKind, SubjectRef,
    TimePoint, TimeValue, TypeError, TzName, Uncertainty, Unit, Zone, ZonedTime,
};
use rahi_types::{Sub, UnixSeconds};

type Outcome = Result<(), String>;

/// 2026-09-17T12:00:00Z.
const NOON: u64 = 1_789_041_600;

const SOURCE: &str = "019c4f00-0000-7000-8000-0000000000a1";

fn dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("testdata/claims")
}

fn subject(kind: &str, key: &str) -> Result<SubjectRef, TypeError> {
    Ok(SubjectRef {
        namespace: Namespace::new("travel")?,
        kind: SubjectKind::new(kind)?,
        key: SubjectKey::new(key)?,
    })
}

fn provenance() -> Result<Provenance, TypeError> {
    let at = UnixSeconds::new(NOON);
    Ok(Provenance::captured(
        SourceRef::new(SourceSystem::new("travel-ingest")?).with_external_id("msg-4411"),
        at,
        UnixSeconds::new(NOON + 4),
    ))
}

fn derived() -> Result<Provenance, TypeError> {
    let source = MemoryId::parse(SOURCE)?;
    let mut provenance = provenance()?;
    provenance.derived_from = vec![source];
    Ok(provenance.with_spans(vec![SourceSpan {
        source,
        part: PartLocator::mime("1.2")?,
        range: SpanRange::new(SpanUnit::Bytes, 2048, 2051)?,
        digest: ContentDigest {
            algorithm: "HMAC-SHA-256".to_owned(),
            key_id: Hex::new("0f".repeat(16))?,
            digest: Hex::new("ab".repeat(32))?,
        },
    }]))
}

fn day(on: (u16, u8, u8)) -> Result<CivilDate, TypeError> {
    CivilDate::ymd(on.0, on.1, on.2)
}

fn warsaw() -> Result<Zone, TypeError> {
    Ok(Zone::Iana(TzName::new("Europe/Warsaw")?))
}

struct Draft {
    id: &'static str,
    subject: SubjectRef,
    predicate: &'static str,
    value: ClaimValue,
    slot: Option<&'static str>,
    epistemic: EpistemicStatus,
    provenance: Provenance,
}

fn claim(draft: Draft) -> Result<Claim, TypeError> {
    Ok(Claim::new(ClaimParts {
        id: ClaimId::parse(draft.id)?,
        scope: Scope::personal(Sub::new("2f1b8c4e-0f6a-4b1e-9a2c-7d3e5f8a1b90")),
        subject: draft.subject,
        predicate: PredicateRef::parse(draft.predicate)?,
        value: draft.value,
        slot: draft.slot.map(SlotKey::new).transpose()?,
        epistemic: draft.epistemic,
        provenance: draft.provenance,
    }))
}

enum Fixture {
    Claim(Box<Claim>),
    Relation(Box<ClaimRelation>),
}

impl Fixture {
    fn render(&self) -> Result<String, String> {
        let text = match self {
            Self::Claim(claim) => serde_json::to_string_pretty(claim),
            Self::Relation(relation) => serde_json::to_string_pretty(relation),
        };
        text.map(|text| format!("{text}\n"))
            .map_err(|error| error.to_string())
    }

    fn reread(&self, text: &str) -> Result<String, String> {
        let again = match self {
            Self::Claim(_) => Self::Claim(Box::new(
                serde_json::from_str(text).map_err(|error| error.to_string())?,
            )),
            Self::Relation(_) => Self::Relation(Box::new(
                serde_json::from_str(text).map_err(|error| error.to_string())?,
            )),
        };
        again.render()
    }
}

/// Every fixture, by file name. Between them: each value type, each zone
/// form, date and time precisions, an uncertainty window, a conditional
/// stance, a slot, a span, and both relation targets.
fn fixtures() -> Result<Vec<(&'static str, Fixture)>, TypeError> {
    let segment = subject("segment", "LO281-2026-10-03")?;
    let booking = subject("booking", "PNR-XK2Q9L")?;
    let stay = subject("stay", "hotel-waw-1")?;
    let plain = |stance| EpistemicStatus::plain(stance);
    let departure = ZonedTime::minute(day((2026, 10, 3))?, 14, 5, warsaw()?)?;
    let window = Uncertainty::new(
        ZonedTime::minute(day((2026, 10, 3))?, 14, 0, Zone::offset(7200)?)?,
        ZonedTime::minute(day((2026, 10, 3))?, 16, 0, Zone::offset(7200)?)?,
    )?;
    let arrival = TimePoint::within(
        TimeValue::DateTime(ZonedTime::hour(
            day((2026, 10, 3))?,
            15,
            Zone::offset(7200)?,
        )?),
        window,
    );

    let mut all = Vec::new();
    let mut add = |name, draft| -> Result<(), TypeError> {
        all.push((name, Fixture::Claim(Box::new(claim(draft)?))));
        Ok(())
    };
    add(
        "departure-iana.json",
        Draft {
            id: "019c4f00-0000-7000-8000-000000000101",
            subject: segment.clone(),
            predicate: "travel:segment.departure@1",
            value: ClaimValue::DateTime(departure),
            slot: None,
            epistemic: plain(Stance::Confirmed)?,
            provenance: provenance()?,
        },
    )?;
    add(
        "boarding-floating.json",
        Draft {
            id: "019c4f00-0000-7000-8000-000000000102",
            subject: segment.clone(),
            predicate: "travel:segment.departure@1",
            value: ClaimValue::DateTime(ZonedTime::second(
                day((2026, 10, 3))?,
                13,
                35,
                0,
                Zone::Floating,
            )?),
            slot: None,
            epistemic: plain(Stance::Observed)?,
            provenance: provenance()?,
        },
    )?;
    add(
        "seat-slot-span.json",
        Draft {
            id: "019c4f00-0000-7000-8000-000000000103",
            subject: segment.clone(),
            predicate: "travel:segment.seat@1",
            value: ClaimValue::Text("14C".to_owned()),
            slot: Some("traveler-1"),
            epistemic: plain(Stance::Requested)?,
            provenance: derived()?,
        },
    )?;
    add(
        "fare-decimal.json",
        Draft {
            id: "019c4f00-0000-7000-8000-000000000104",
            subject: booking.clone(),
            predicate: "travel:booking.fare@1",
            value: ClaimValue::Decimal(Decimal::parse(
                "1234.50",
                Some(Unit::Currency(CurrencyCode::new("EUR")?)),
            )?),
            slot: None,
            epistemic: plain(Stance::Asserted)?,
            provenance: provenance()?,
        },
    )?;
    add(
        "stay-interval-conditional.json",
        Draft {
            id: "019c4f00-0000-7000-8000-000000000105",
            subject: stay.clone(),
            predicate: "travel:stay.dates@1",
            value: ClaimValue::Interval(Interval::new(
                Bound::Inclusive(TimePoint::exact(TimeValue::Date(day((2026, 10, 3))?))),
                Bound::Exclusive(TimePoint::exact(TimeValue::Date(day((2026, 10, 6))?))),
            )?),
            slot: None,
            epistemic: EpistemicStatus::conditional("if the deposit is paid")?,
            provenance: provenance()?,
        },
    )?;
    add(
        "arrival-window-open-end.json",
        Draft {
            id: "019c4f00-0000-7000-8000-000000000106",
            subject: segment.clone(),
            predicate: "travel:segment.arrival_window@1",
            value: ClaimValue::Interval(Interval::new(Bound::Inclusive(arrival), Bound::Open)?),
            slot: None,
            epistemic: plain(Stance::Estimated)?,
            provenance: provenance()?,
        },
    )?;
    add(
        "status-enum.json",
        Draft {
            id: "019c4f00-0000-7000-8000-000000000107",
            subject: booking.clone(),
            predicate: "travel:booking.status@1",
            value: ClaimValue::Enum(EnumVariant::new("ticketed")?),
            slot: None,
            epistemic: plain(Stance::Confirmed)?,
            provenance: provenance()?,
        },
    )?;
    add(
        "segment-booking-ref.json",
        Draft {
            id: "019c4f00-0000-7000-8000-000000000108",
            subject: segment,
            predicate: "travel:segment.booking@1",
            value: ClaimValue::EntityRef(booking.clone()),
            slot: None,
            epistemic: plain(Stance::Asserted)?,
            provenance: provenance()?,
        },
    )?;
    add(
        "trip-month-denied.json",
        Draft {
            id: "019c4f00-0000-7000-8000-000000000109",
            subject: booking,
            predicate: "travel:booking.travel_month@1",
            value: ClaimValue::Date(CivilDate::ym(2026, 10)?),
            slot: None,
            epistemic: plain(Stance::Denied)?,
            provenance: provenance()?,
        },
    )?;
    add(
        "bags-integer-expected.json",
        Draft {
            id: "019c4f00-0000-7000-8000-00000000010a",
            subject: stay,
            predicate: "travel:ancillary.bags@2",
            value: ClaimValue::Integer(2),
            slot: None,
            epistemic: plain(Stance::Expected)?,
            provenance: provenance()?,
        },
    )?;

    let from = ClaimId::parse("019c4f00-0000-7000-8000-000000000102")?;
    all.push((
        "relation-supersedes.json",
        Fixture::Relation(Box::new(ClaimRelation::new(
            from,
            RelationKind::Supersedes,
            RelationTarget::Claim(ClaimId::parse("019c4f00-0000-7000-8000-000000000101")?),
            provenance()?,
        )?)),
    ));
    all.push((
        "relation-derived-from-memory.json",
        Fixture::Relation(Box::new(ClaimRelation::new(
            ClaimId::parse("019c4f00-0000-7000-8000-000000000103")?,
            RelationKind::DerivedFrom,
            RelationTarget::Memory(MemoryId::parse(SOURCE)?),
            provenance()?,
        )?)),
    ));
    Ok(all)
}

/// FR-001, AC-1: byte-identical round trips.
#[test]
fn fr001_every_claim_fixture_round_trips() -> Outcome {
    let fixtures = fixtures().map_err(|error| error.to_string())?;
    let overwrite = std::env::var("AICORTEX_FIXTURES").as_deref() == Ok("overwrite");
    let mut on_disk: Vec<String> = fs::read_dir(dir())
        .map_err(|error| error.to_string())?
        .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
        .collect();
    on_disk.sort();
    let mut expected: Vec<String> = fixtures
        .iter()
        .map(|(name, _)| (*name).to_owned())
        .collect();
    expected.sort();
    if !overwrite && on_disk != expected {
        return Err(format!(
            "testdata/claims holds {on_disk:?}, the fixtures are {expected:?}"
        ));
    }
    for (name, fixture) in &fixtures {
        let path = dir().join(name);
        let rendered = fixture.render()?;
        if overwrite {
            fs::write(&path, &rendered).map_err(|error| error.to_string())?;
            continue;
        }
        let text = fs::read_to_string(&path).map_err(|error| format!("{name}: {error}"))?;
        if text != rendered {
            return Err(format!("{name} is not what the code emits"));
        }
        if fixture.reread(&text)? != text {
            return Err(format!("{name} does not round-trip byte for byte"));
        }
    }
    Ok(())
}

fn refused_naming<T: for<'de> serde::Deserialize<'de>>(json: &str, field: &str) -> Outcome {
    match serde_json::from_str::<T>(json) {
        Ok(_) => Err(format!("{json} was accepted")),
        Err(error) if error.to_string().starts_with(&format!("{field}: ")) => Ok(()),
        Err(error) => Err(format!("{json}: the error does not name {field}: {error}")),
    }
}

/// FR-002: unknown discriminants are typed errors naming the field.
#[test]
fn fr002_an_unknown_discriminant_names_its_field() -> Outcome {
    refused_naming::<ClaimValue>(r#"{"type":"float","float":1.5}"#, "type")?;
    refused_naming::<Zone>(r#"{"kind":"utc"}"#, "zone")?;
    refused_naming::<CivilDate>(r#"{"on":"2026-10","precision":"week"}"#, "precision")?;
    refused_naming::<ZonedTime>(
        r#"{"local":"2026-10-03T14","precision":"minute","zone":{"kind":"floating"}}"#,
        "precision",
    )?;
    refused_naming::<EpistemicStatus>(r#"{"stance":"rumoured"}"#, "stance")?;
    refused_naming::<RelationKind>(r#""refutes""#, "relation")?;
    refused_naming::<RelationTarget>(
        r#"{"kind":"entity","id":"019c4f00-0000-7000-8000-000000000101"}"#,
        "target",
    )?;
    refused_naming::<Unit>(r#"{"kind":"weight","name":"kg"}"#, "unit")?;
    refused_naming::<PartLocator>(r#"{"kind":"header"}"#, "part")?;
    refused_naming::<SpanRange>(r#"{"unit":"words","start":0,"end":3}"#, "unit")?;
    Ok(())
}

/// FR-002 and B-5: shapes the types refuse even with known discriminants.
#[test]
fn b005_values_hold_their_shape() -> Outcome {
    refused_naming::<CivilDate>(r#"{"on":"2026-02-29","precision":"day"}"#, "date")?;
    refused_naming::<Decimal>(r#"{"amount":"-0.00"}"#, "amount")?;
    refused_naming::<Decimal>(r#"{"amount":"1e3"}"#, "amount")?;
    refused_naming::<Decimal>(r#"{"amount":"007"}"#, "amount")?;
    refused_naming::<ClaimValue>(r#"{"type":"text","integer":3}"#, "type")?;
    refused_naming::<EpistemicStatus>(r#"{"stance":"conditional"}"#, "condition")?;
    refused_naming::<Zone>(r#"{"kind":"offset","seconds":90000}"#, "zone")?;
    refused_naming::<Interval>(
        r#"{"start":{"kind":"inclusive","point":{"kind":"date","date":{"on":"2026-10-06","precision":"day"}}},"end":{"kind":"inclusive","point":{"kind":"date","date":{"on":"2026-10-03","precision":"day"}}}}"#,
        "interval",
    )?;
    let memory = MemoryId::parse(SOURCE).map_err(|error| error.to_string())?;
    let from = ClaimId::parse("019c4f00-0000-7000-8000-000000000101")
        .map_err(|error| error.to_string())?;
    let supersedes_memory = ClaimRelation::new(
        from,
        RelationKind::Supersedes,
        RelationTarget::Memory(memory),
        provenance().map_err(|error| error.to_string())?,
    );
    if supersedes_memory.is_ok() {
        return Err("a memory was accepted as the target of supersedes".to_owned());
    }
    Ok(())
}

/// FR-003: the unconstructible cases do not compile.
#[test]
fn fr003_a_claim_needs_its_provenance() {
    let cases = trybuild::TestCases::new();
    cases.compile_fail("tests/claim_compile_fail/*.rs");
}

/// FR-006: a decimal is exact and compares equal to itself across
/// serialization; its scale is kept.
#[test]
fn fr006_a_decimal_is_exact() -> Outcome {
    for text in [
        "0",
        "0.10",
        "-12.345",
        "1234.50",
        "170141183460469231731687303715884105727",
    ] {
        let decimal = Decimal::parse(text, None).map_err(|error| error.to_string())?;
        if decimal.amount() != text {
            return Err(format!("{text} renders as {}", decimal.amount()));
        }
        let wire = serde_json::to_string(&decimal).map_err(|error| error.to_string())?;
        let back: Decimal = serde_json::from_str(&wire).map_err(|error| error.to_string())?;
        if back != decimal {
            return Err(format!("{text} is not equal to itself after {wire}"));
        }
    }
    let tenth = Decimal::parse("0.1", None).map_err(|error| error.to_string())?;
    if (tenth.mantissa(), tenth.scale()) != (1, 1) {
        return Err("0.1 is not mantissa 1 at scale 1".to_owned());
    }
    let one_and_half = Decimal::parse("1.5", None).map_err(|error| error.to_string())?;
    let with_zero = Decimal::parse("1.50", None).map_err(|error| error.to_string())?;
    if one_and_half == with_zero {
        return Err("the scale a source gave was not kept".to_owned());
    }
    Ok(())
}
