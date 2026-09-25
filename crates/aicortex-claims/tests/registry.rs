//! Claims against a registry, checked (spec 050).
//!
//! - FR-004, AC-2: over `testdata/registries/`, `validate` accepts each valid
//!   claim and refuses each invalid one with the recorded code.
//! - FR-005: a later version that changes a value type or a cardinality is
//!   refused, the same version with different content is refused, and the
//!   same version with identical content is a no-op.
//! - FR-007: the travel fixture declares `segment`, `booking` and `traveler`
//!   and a departure, a seat, a fare, a stay, a status and a booking
//!   reference, and every one validates (the `ok` cases of `cases.json`).
//! - AC-4: the crate reaches no I/O, SQL or async crate.

// The FR-005 cases edit a fixture document by key; a missing key is a
// broken fixture, which the assertions that follow report.
#![allow(clippy::indexing_slicing)]

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use aicortex_claims::{Admission, Change, RegistryError, RegistrySnapshot, validate};
use aicortex_types::{
    Claim, ClaimId, ClaimParts, ClaimValue, ContentDigest, EpistemicStatus, Hex, MemoryId,
    Namespace, PartLocator, PredicateRef, PredicateSet, Provenance, Scope, SlotKey, SourceRef,
    SourceSpan, SourceSystem, SpanRange, SpanUnit, SubjectKey, SubjectKind, SubjectRef,
};
use rahi_types::{Sub, UnixSeconds};
use serde::Deserialize;

type Outcome = Result<(), String>;

fn dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("testdata/registries")
}

fn read<T: for<'de> Deserialize<'de>>(name: &str) -> Result<T, String> {
    let text = fs::read_to_string(dir().join(name)).map_err(|error| format!("{name}: {error}"))?;
    serde_json::from_str(&text).map_err(|error| format!("{name}: {error}"))
}

#[derive(Deserialize)]
struct Cases {
    registries: Vec<String>,
    cases: Vec<Case>,
}

#[derive(Deserialize)]
struct CaseSubject {
    kind: String,
    key: String,
}

#[derive(Deserialize)]
struct Case {
    name: String,
    #[allow(dead_code)]
    why: String,
    predicate: String,
    subject: CaseSubject,
    value: ClaimValue,
    epistemic: EpistemicStatus,
    #[serde(default)]
    slot: Option<String>,
    /// `derived` for a span into the memory the claim derives from,
    /// `elsewhere` for a span into another.
    #[serde(default)]
    span_source: Option<String>,
    expect: String,
}

const SOURCE: &str = "019c4f00-0000-7000-8000-0000000000a1";
const ELSEWHERE: &str = "019c4f00-0000-7000-8000-0000000000b2";

fn claim(case: &Case) -> Result<Claim, String> {
    let fail = |error: aicortex_types::TypeError| format!("{}: {error}", case.name);
    let predicate = PredicateRef::parse(&case.predicate).map_err(fail)?;
    let at = UnixSeconds::new(1_789_041_600);
    let mut provenance = Provenance::captured(
        SourceRef::new(SourceSystem::new("travel-ingest").map_err(fail)?),
        at,
        at,
    );
    if let Some(span) = &case.span_source {
        let source = MemoryId::parse(SOURCE).map_err(fail)?;
        let target = if span == "derived" {
            source
        } else {
            MemoryId::parse(ELSEWHERE).map_err(fail)?
        };
        provenance.derived_from = vec![source];
        provenance = provenance.with_spans(vec![SourceSpan {
            source: target,
            part: PartLocator::Body,
            range: SpanRange::new(SpanUnit::Chars, 120, 126).map_err(fail)?,
            digest: ContentDigest {
                algorithm: "HMAC-SHA-256".to_owned(),
                key_id: Hex::new("0f".repeat(16)).map_err(fail)?,
                digest: Hex::new("ab".repeat(32)).map_err(fail)?,
            },
        }]);
    }
    Ok(Claim::new(ClaimParts {
        id: ClaimId::parse("019c4f00-0000-7000-8000-0000000000c3").map_err(fail)?,
        scope: Scope::personal(Sub::new("2f1b8c4e-0f6a-4b1e-9a2c-7d3e5f8a1b90")),
        subject: SubjectRef {
            namespace: predicate.namespace.clone(),
            kind: SubjectKind::new(case.subject.kind.clone()).map_err(fail)?,
            key: SubjectKey::new(case.subject.key.clone()).map_err(fail)?,
        },
        predicate,
        value: case.value.clone(),
        slot: case
            .slot
            .clone()
            .map(SlotKey::new)
            .transpose()
            .map_err(fail)?,
        epistemic: case.epistemic.clone(),
        provenance,
    }))
}

fn snapshot(names: &[String]) -> Result<RegistrySnapshot, String> {
    let sets = names
        .iter()
        .map(|name| read::<PredicateSet>(name))
        .collect::<Result<Vec<_>, _>>()?;
    RegistrySnapshot::from_sets(sets).map_err(|error| error.to_string())
}

/// FR-004, FR-007: every case answers what it records.
#[test]
fn fr004_every_case_answers_as_recorded() -> Outcome {
    let cases: Cases = read("cases.json")?;
    let registry = snapshot(&cases.registries)?;
    let mut failures = Vec::new();
    for case in &cases.cases {
        let answer = match validate(&claim(case)?, &registry) {
            Ok(_) => "ok".to_owned(),
            Err(error) => error.code().to_owned(),
        };
        if answer != case.expect {
            failures.push(format!(
                "{}: expected {}, got {answer}",
                case.name, case.expect
            ));
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("\n"))
    }
}

/// FR-004: every refusal code the spec lists has a case.
#[test]
fn fr004_the_corpus_covers_every_listed_refusal() -> Outcome {
    let cases: Cases = read("cases.json")?;
    for code in [
        "unknown_predicate",
        "wrong_version",
        "wrong_subject_kind",
        "wrong_value_type",
        "undeclared_unit",
        "undeclared_variant",
        "missing_slot",
        "inadmissible_status",
    ] {
        if !cases.cases.iter().any(|case| case.expect == code) {
            return Err(format!("no case expects {code}"));
        }
    }
    Ok(())
}

/// FR-007: the travel fixture declares the subject kinds and the six value
/// shapes the first consumer needs.
#[test]
fn fr007_the_travel_registry_declares_what_travel_needs() -> Outcome {
    let v1: PredicateSet = read("travel-v1.json")?;
    for kind in ["segment", "booking", "traveler"] {
        let kind = SubjectKind::new(kind).map_err(|error| error.to_string())?;
        if !v1.subject_kinds().contains(&kind) {
            return Err(format!("travel-v1 does not declare {kind}"));
        }
    }
    let cases: Cases = read("cases.json")?;
    for name in [
        "departure",
        "seat",
        "fare",
        "stay",
        "status",
        "segment-booking",
    ] {
        let found = cases
            .cases
            .iter()
            .any(|case| case.name == name && case.expect == "ok");
        if !found {
            return Err(format!("no validating case {name}"));
        }
    }
    Ok(())
}

fn edited(edit: impl FnOnce(&mut serde_json::Value)) -> Result<PredicateSet, String> {
    let text =
        fs::read_to_string(dir().join("travel-v1.json")).map_err(|error| error.to_string())?;
    let mut document: serde_json::Value =
        serde_json::from_str(&text).map_err(|error| error.to_string())?;
    edit(&mut document);
    serde_json::from_value(document).map_err(|error| error.to_string())
}

fn predicate_mut<'a>(
    document: &'a mut serde_json::Value,
    name: &str,
) -> Option<&'a mut serde_json::Value> {
    document
        .get_mut("predicates")?
        .as_array_mut()?
        .iter_mut()
        .find(|def| def.get("name").and_then(serde_json::Value::as_str) == Some(name))
}

fn refusal(registry: &RegistrySnapshot, set: &PredicateSet) -> Result<RegistryError, String> {
    match registry.check(set) {
        Ok(admission) => Err(format!("{admission:?} where a refusal was expected")),
        Err(error) => Ok(error),
    }
}

/// FR-005: immutability and the successor rule.
#[test]
fn fr005_a_version_is_immutable_and_a_successor_keeps_meaning() -> Outcome {
    let v1: PredicateSet = read("travel-v1.json")?;
    let mut registry = RegistrySnapshot::new();
    if registry.admit(v1.clone()) != Ok(Admission::New) {
        return Err("version 1 was not admitted to an empty registry".to_owned());
    }
    if registry.check(&v1) != Ok(Admission::Unchanged) {
        return Err("identical content under a registered version is not a no-op".to_owned());
    }

    let different = edited(|document| {
        if let Some(def) = predicate_mut(document, "booking.locator") {
            def["epistemic"] = serde_json::json!(["asserted", "confirmed"]);
        }
    })?;
    let error = refusal(&registry, &different)?;
    if !matches!(error, RegistryError::ConflictingVersion { version: 1 }) {
        return Err(format!("different content under version 1: {error}"));
    }

    let retyped = edited(|document| {
        document["version"] = serde_json::json!(2);
        if let Some(def) = predicate_mut(document, "segment.departure") {
            def["value"] = serde_json::json!({ "type": "text" });
        }
    })?;
    let error = refusal(&registry, &retyped)?;
    if !matches!(
        error,
        RegistryError::Incompatible {
            change: Change::ValueType,
            ..
        }
    ) {
        return Err(format!("a changed value type: {error}"));
    }

    let recounted = edited(|document| {
        document["version"] = serde_json::json!(2);
        if let Some(def) = predicate_mut(document, "segment.seat") {
            def["cardinality"] = serde_json::json!("one");
            if let Some(object) = def.as_object_mut() {
                object.remove("slot");
            }
        }
    })?;
    let error = refusal(&registry, &recounted)?;
    if !matches!(
        error,
        RegistryError::Incompatible {
            change: Change::Cardinality,
            ..
        }
    ) {
        return Err(format!("a changed cardinality: {error}"));
    }

    let narrowed = edited(|document| {
        document["version"] = serde_json::json!(2);
        if let Some(def) = predicate_mut(document, "booking.status") {
            def["value"]["variants"] = serde_json::json!(["held", "ticketed"]);
        }
    })?;
    let error = refusal(&registry, &narrowed)?;
    if !matches!(
        error,
        RegistryError::Incompatible {
            change: Change::ValueType,
            ..
        }
    ) {
        return Err(format!("a dropped variant: {error}"));
    }

    let v2: PredicateSet = read("travel-v2.json")?;
    if registry.admit(v2) != Ok(Admission::New) {
        return Err("the additive version 2 was not admitted".to_owned());
    }
    let error = refusal(&registry, &retyped)?;
    if !matches!(error, RegistryError::ConflictingVersion { version: 2 }) {
        return Err(format!("a second, different version 2: {error}"));
    }
    Ok(())
}

/// B-9: the `aicortex` namespace is reserved.
#[test]
fn b009_the_aicortex_namespace_is_reserved() -> Outcome {
    let reserved = edited(|document| document["namespace"] = serde_json::json!("aicortex"))?;
    let namespace = Namespace::new("aicortex").map_err(|error| error.to_string())?;
    if !namespace.is_reserved() {
        return Err("aicortex is not reported reserved".to_owned());
    }
    match RegistrySnapshot::new().check(&reserved) {
        Err(RegistryError::ReservedNamespace) => Ok(()),
        other => Err(format!("a document in the reserved namespace: {other:?}")),
    }
}

/// AC-4: no I/O, SQL, or async crate, over the resolved normal-edge tree.
#[test]
fn ac004_the_crate_is_pure() -> Outcome {
    const FORBIDDEN: [&str; 17] = [
        "tokio",
        "async-std",
        "smol",
        "async-trait",
        "futures-executor",
        "mio",
        "socket2",
        "hyper",
        "axum",
        "reqwest",
        "tower",
        "hiqlite",
        "rusqlite",
        "libsqlite3-sys",
        "sqlx",
        "rahi-store",
        "aicortex-store",
    ];
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    let output = Command::new(cargo)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .args([
            "tree",
            "--locked",
            "--package",
            "aicortex-claims",
            "--edges",
            "normal",
            "--prefix",
            "none",
            "--format",
            "{p}",
        ])
        .output()
        .map_err(|error| format!("cargo tree: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "cargo tree: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let tree = String::from_utf8_lossy(&output.stdout);
    let found: Vec<&str> = tree
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .filter(|name| FORBIDDEN.contains(name))
        .collect();
    if found.is_empty() {
        Ok(())
    } else {
        Err(format!("aicortex-claims reaches {found:?}"))
    }
}
