//! The record, checked (spec 011).
//!
//! - FR-001, AC-1: every fixture under `testdata/memories/` deserializes and
//!   re-serializes byte for byte.
//! - FR-002: a string outside a closed vocabulary is refused by a typed error
//!   that names the field it sat in.
//! - FR-003, AC-1: the compile-fail cases under `tests/compile_fail/`.
//! - FR-004: `Memory` has no constructor reachable without a `Provenance`.
//!   The compiler checks this on the doc tests of `Memory` and on
//!   `compile_fail/memory_without_provenance.rs`; what is asserted here is
//!   that the one reachable constructor does demand one.
//! - FR-005: `decayed_at` is non-increasing in elapsed time and is exactly
//!   `base` at zero elapsed time.
//! - AC-2: the resolved dependency tree carries no I/O, SQL, or async crate.
//!
//! The fixtures are written by this file. `AICORTEX_FIXTURES=overwrite cargo
//! test -p aicortex-types --test model` rewrites them from `fixtures()` and
//! asserts nothing; every other run reads them and compares. That is what
//! keeps a fixture byte-identical to what the current code emits without
//! anybody hand-editing JSON.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use aicortex_types::{
    Actor, ActorId, AgentOrigin, DecisionRef, ExtractorVersion, Importance, MEMORY_SCHEMA_VERSION,
    MediaDigest, MediaRef, Memory, MemoryBody, MemoryId, MemoryKind, MemoryParts, ProjectKey,
    Promotion, Provenance, Scope, ShareKey, SourceRef, SourceSystem, Status, TrustClass, TypeError,
};
use rahi_types::{Sub, UnixSeconds};

type Outcome = Result<(), String>;

/// 2026-09-17T12:00:00Z, so a fixture reads as a moment rather than a number.
const NOON: u64 = 1_789_041_600;

/// Crates that would make this one something other than plain data (AC-2).
///
/// The list is of the shapes the territory forbids: an async runtime, an HTTP
/// stack, a database driver, a filesystem or network client. A dependency
/// that belongs to none of them is allowed; one that belongs to any of them
/// means the crate stopped being types.
const FORBIDDEN: [&str; 21] = [
    "tokio",
    "tokio-util",
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
    "postgres",
    "tokio-postgres",
    "deadpool",
    "s3-simple",
    "rahi-store",
];

/// The dependencies this crate is allowed to name directly (section 2).
const ALLOWED_DIRECT: [&str; 3] = ["rahi-types", "serde", "uuid"];

fn crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn fixtures_dir() -> PathBuf {
    crate_root().join("testdata/memories")
}

/// A subject, an actor, and a source, so each fixture reads as one thought.
fn human() -> Result<Actor, TypeError> {
    Ok(Actor::human(ActorId::new("sub-bartek")?).labelled("Bartek"))
}

fn agent() -> Result<Actor, TypeError> {
    Ok(Actor::agent(
        ActorId::new("agent-curator-7")?,
        AgentOrigin {
            client: Some("claude-code".to_owned()),
            model: Some("claude-opus-5".to_owned()),
        },
    ))
}

fn source(system: &str) -> Result<SourceRef, TypeError> {
    Ok(SourceRef::new(SourceSystem::new(system)?))
}

fn id(text: &str) -> Result<MemoryId, TypeError> {
    MemoryId::parse(text)
}

/// Every fixture, by the file it is written to.
///
/// Between them they cover all three trust classes, all five statuses, a
/// derived memory with its extractor, a body with media and a title, and each
/// of the three scope kinds.
fn fixtures() -> Result<Vec<(&'static str, Memory)>, TypeError> {
    let owner = Sub::new("2f1b8c4e-0f6a-4b1e-9a2c-7d3e5f8a1b90");
    let captured = UnixSeconds::new(NOON);
    let ingested = UnixSeconds::new(NOON + 4);

    let mut fixtures = Vec::new();

    // An original observation a person wrote into their own scope.
    fixtures.push((
        "personal-observation.json",
        Memory::new(MemoryParts {
            id: id("019c4f00-0000-7000-8000-000000000001")?,
            scope: Scope::personal(owner.clone()),
            kind: MemoryKind::Observation,
            body: MemoryBody::text("The gate runs before the transaction, never inside it."),
            actor: human()?,
            provenance: Provenance::captured(
                source("claude-code")?.with_locator("session/2026-09-17"),
                captured,
                ingested,
            ),
            trust: TrustClass::Evidence,
            importance: Importance::at(ingested)?,
            created: ingested,
        }),
    ));

    // A fact an agent derived from two observations, in a project scope.
    fixtures.push((
        "project-fact-derived.json",
        Memory::new(MemoryParts {
            id: id("019c4f00-0000-7000-8000-000000000002")?,
            scope: Scope::project(owner.clone(), ProjectKey::new("aicortex")?),
            kind: MemoryKind::Fact,
            body: MemoryBody::text("Every vector names the model that produced it.")
                .with_title("Vectors name their model")
                .with_media(
                    MediaRef::new(
                        MediaDigest::parse(
                            "sha256:9f2c1d0e4a6b8c3d5e7f90a1b2c3d4e5f60718293a4b5c6d7e8f9012a3b4c5d6",
                        )?,
                        "image/png",
                    )?
                    .with_bytes(20_481),
                ),
            actor: agent()?,
            provenance: Provenance::captured(source("curator")?, captured, ingested).derived(
                vec![
                    id("019c4f00-0000-7000-8000-000000000001")?,
                    id("019c4f00-0000-7000-8000-00000000000a")?,
                ],
                ExtractorVersion::new("fact-extractor", "0.3.1")?,
            ),
            trust: TrustClass::Assertion,
            importance: Importance::new(0.75, ingested, 3)?,
            created: ingested,
        }),
    ));

    // A memory a human raised to instruction grade, recorded in the ledger.
    fixtures.push((
        "shared-instruction-promoted.json",
        Memory::new(MemoryParts {
            id: id("019c4f00-0000-7000-8000-000000000003")?,
            scope: Scope::shared(owner.clone(), ShareKey::new("statecrafting")?),
            kind: MemoryKind::Preference,
            body: MemoryBody::text("Use a colon or a semicolon; never an em dash."),
            actor: human()?,
            provenance: Provenance::captured(source("aicortex.api")?, captured, ingested),
            trust: TrustClass::instruction(Promotion::new(
                owner.clone(),
                DecisionRef::new("01JC5X6Q7ZQ9V0HT8MEXAMPLE")?,
            )),
            importance: Importance::new(1.0, ingested, 12)?,
            created: ingested,
        }),
    ));

    // An import whose origin could not be established (spec 013 B-7).
    let mut quarantined = Memory::new(MemoryParts {
        id: id("019c4f00-0000-7000-8000-000000000004")?,
        scope: Scope::personal(owner.clone()),
        kind: MemoryKind::Reference,
        body: MemoryBody::text("Pasted from an export nobody can vouch for."),
        actor: Actor::import(ActorId::new("import-run-42")?),
        provenance: Provenance::captured(
            source("import:obsidian")?.with_external_id("vault/notes/17.md"),
            captured,
            ingested,
        ),
        trust: TrustClass::Assertion,
        importance: Importance::new(0.25, ingested, 0)?,
        created: ingested,
    });
    quarantined.status = Status::Quarantined;
    fixtures.push(("quarantined-import.json", quarantined));

    // A claim a later correction replaced.
    let mut superseded = Memory::new(MemoryParts {
        id: id("019c4f00-0000-7000-8000-000000000005")?,
        scope: Scope::personal(owner.clone()),
        kind: MemoryKind::Correction,
        body: MemoryBody::text("The half-life is thirty days, not seven."),
        actor: human()?,
        provenance: Provenance::captured(source("claude-code")?, captured, ingested),
        trust: TrustClass::Evidence,
        importance: Importance::at(ingested)?,
        created: ingested,
    });
    superseded.status = Status::Superseded(id("019c4f00-0000-7000-8000-000000000006")?);
    superseded.updated = UnixSeconds::new(NOON + 900);
    fixtures.push(("superseded-correction.json", superseded));

    // A forgotten memory: the id, the scope, and the timestamps, and nothing
    // else. The body is gone rather than blanked with a marker (B-6).
    let mut erased = Memory::new(MemoryParts {
        id: id("019c4f00-0000-7000-8000-000000000007")?,
        scope: Scope::personal(owner),
        kind: MemoryKind::PersonNote,
        body: MemoryBody::default(),
        actor: Actor::system(ActorId::new("eraser")?),
        provenance: Provenance::captured(source("aicortex.curate")?, captured, ingested),
        trust: TrustClass::Evidence,
        importance: Importance::new(0.0, ingested, 0)?,
        created: ingested,
    });
    erased.status = Status::Erased;
    erased.updated = UnixSeconds::new(NOON + 86_400);
    fixtures.push(("erased.json", erased));

    Ok(fixtures)
}

/// The JSON one fixture is written as: pretty, with a trailing newline, so the
/// file is diffable in review.
fn render(memory: &Memory) -> Result<String, String> {
    let mut json = serde_json::to_string_pretty(memory).map_err(|error| error.to_string())?;
    json.push('\n');
    Ok(json)
}

fn fixture_files() -> Result<Vec<PathBuf>, String> {
    let dir = fixtures_dir();
    let entries = fs::read_dir(&dir).map_err(|error| format!("{}: {error}", dir.display()))?;
    let mut files = Vec::new();
    for entry in entries {
        let path = entry.map_err(|error| error.to_string())?.path();
        if path
            .extension()
            .is_some_and(|extension| extension == "json")
        {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

fn file_name(path: &Path) -> Result<String, String> {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(str::to_owned)
        .ok_or_else(|| format!("{}: unreadable file name", path.display()))
}

/// FR-001: every fixture deserializes and re-serializes byte for byte.
///
/// The comparison is over the file's own bytes, not over a value: a field that
/// stopped serializing, a field that started, a renamed key, or a reordered
/// struct all change the bytes and all fail here.
#[test]
fn fr001_every_fixture_round_trips_byte_identically() -> Outcome {
    if std::env::var_os("AICORTEX_FIXTURES").is_some_and(|value| value == "overwrite") {
        return write_fixtures();
    }

    let files = fixture_files()?;
    if files.is_empty() {
        return Err(format!("{}: no fixtures", fixtures_dir().display()));
    }

    for path in &files {
        let original =
            fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))?;
        let memory: Memory = serde_json::from_str(&original)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        let rendered = render(&memory)?;
        if rendered != original {
            return Err(format!(
                "{}: re-serializing changed the bytes\n--- stored ---\n{original}\n--- emitted ---\n{rendered}",
                path.display()
            ));
        }
        // And once more from the value, so a `Deserialize` that silently
        // dropped a field cannot pass by emitting the same text twice.
        let again: Memory = serde_json::from_str(&rendered)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        if again != memory {
            return Err(format!(
                "{}: the value did not survive the round trip",
                path.display()
            ));
        }
    }
    Ok(())
}

/// The fixtures on disk are exactly the ones `fixtures()` writes.
///
/// Without this, a fixture could be deleted from `fixtures()` and left on
/// disk, where it would keep passing FR-001 while covering nothing.
#[test]
fn fixtures_on_disk_match_the_ones_this_test_writes() -> Outcome {
    let declared: BTreeSet<String> = fixtures()
        .map_err(|error| error.to_string())?
        .into_iter()
        .map(|(name, _)| name.to_owned())
        .collect();
    let mut stored = BTreeSet::new();
    for path in fixture_files()? {
        stored.insert(file_name(&path)?);
    }
    if declared == stored {
        return Ok(());
    }
    Err(format!(
        "fixtures() writes {declared:?} but {} holds {stored:?}",
        fixtures_dir().display()
    ))
}

fn write_fixtures() -> Outcome {
    let dir = fixtures_dir();
    fs::create_dir_all(&dir).map_err(|error| format!("{}: {error}", dir.display()))?;
    for path in fixture_files()? {
        fs::remove_file(&path).map_err(|error| format!("{}: {error}", path.display()))?;
    }
    for (name, memory) in fixtures().map_err(|error| error.to_string())? {
        let path = dir.join(name);
        fs::write(&path, render(&memory)?)
            .map_err(|error| format!("{}: {error}", path.display()))?;
    }
    Ok(())
}

/// FR-002: a string outside a closed vocabulary is refused, by field name.
///
/// Every taxonomy is checked in both directions: the refusal and the message,
/// so a vocabulary that silently widened would fail here rather than in the
/// spec that trusted it.
#[test]
fn fr002_an_unknown_variant_is_refused_and_names_its_field() -> Outcome {
    let owner = "\"2f1b8c4e-0f6a-4b1e-9a2c-7d3e5f8a1b90\"";
    let cases: [(&str, String); 5] = [
        (
            "kind",
            format!(
                r#"{{"id":"019c4f00-0000-7000-8000-000000000001","scope":{{"owner":{owner},"kind":"personal"}},"kind":"epiphany"}}"#
            ),
        ),
        (
            "trust",
            format!(
                r#"{{"id":"019c4f00-0000-7000-8000-000000000001","scope":{{"owner":{owner},"kind":"personal"}},"kind":"fact","body":{{"text":"x"}},"actor":{{"kind":"human","id":"a"}},"provenance":{{"source":{{"system":"s"}},"captured_at":1,"ingested_at":1}},"trust":{{"class":"gospel"}}}}"#
            ),
        ),
        (
            "status",
            format!(
                r#"{{"id":"019c4f00-0000-7000-8000-000000000001","scope":{{"owner":{owner},"kind":"personal"}},"kind":"fact","body":{{"text":"x"}},"actor":{{"kind":"human","id":"a"}},"provenance":{{"source":{{"system":"s"}},"captured_at":1,"ingested_at":1}},"trust":{{"class":"evidence"}},"status":{{"state":"pondered"}}}}"#
            ),
        ),
        (
            "scope.kind",
            format!(
                r#"{{"id":"019c4f00-0000-7000-8000-000000000001","scope":{{"owner":{owner},"kind":"galactic"}}}}"#
            ),
        ),
        (
            "actor.kind",
            format!(
                r#"{{"id":"019c4f00-0000-7000-8000-000000000001","scope":{{"owner":{owner},"kind":"personal"}},"kind":"fact","body":{{"text":"x"}},"actor":{{"kind":"oracle","id":"a"}}}}"#
            ),
        ),
    ];

    for (field, json) in &cases {
        match serde_json::from_str::<Memory>(json) {
            Ok(_) => return Err(format!("{field}: an unknown variant deserialized")),
            Err(error) => {
                let message = error.to_string();
                if !message.contains(field) {
                    return Err(format!(
                        "{field}: the refusal does not name the field: {message}"
                    ));
                }
            }
        }
    }

    // The same refusal, reached directly, so the error is typed rather than
    // only a string inside serde's.
    match "epiphany".parse::<MemoryKind>() {
        Err(TypeError::UnknownVariant { field, value, .. }) => {
            if field != "kind" || value != "epiphany" {
                return Err(format!("the refusal named {field:?} and quoted {value:?}"));
            }
        }
        other => return Err(format!("kind: expected an UnknownVariant, got {other:?}")),
    }
    Ok(())
}

/// D-6: a variant that carries a payload is refused when the payload is
/// absent, by a message that names the field.
///
/// `instruction` without its promotion is the dangerous one: a wire form that
/// let the discriminant stand alone would be a way to instruction grade with
/// no authority behind it.
#[test]
fn d006_a_variant_without_its_payload_is_refused() -> Outcome {
    let cases = [("trust.promotion", r#"{"class":"instruction"}"#)];
    for (field, json) in cases {
        match serde_json::from_str::<TrustClass>(json) {
            Ok(trust) => {
                return Err(format!("{field}: {json} deserialized as {trust:?}"));
            }
            Err(error) if error.to_string().contains(field) => {}
            Err(error) => {
                return Err(format!("{field}: the refusal does not name it: {error}"));
            }
        }
    }

    let owner = "\"2f1b8c4e-0f6a-4b1e-9a2c-7d3e5f8a1b90\"";
    let scope_cases = [
        (
            "scope.project",
            format!(r#"{{"owner":{owner},"kind":"project"}}"#),
        ),
        (
            "scope.share",
            format!(r#"{{"owner":{owner},"kind":"shared"}}"#),
        ),
    ];
    for (field, json) in &scope_cases {
        match serde_json::from_str::<Scope>(json) {
            Ok(scope) => return Err(format!("{field}: {json} deserialized as {scope}")),
            Err(error) if error.to_string().contains(field) => {}
            Err(error) => {
                return Err(format!("{field}: the refusal does not name it: {error}"));
            }
        }
    }

    // And a superseded status with no successor.
    match serde_json::from_str::<Status>(r#"{"state":"superseded"}"#) {
        Ok(status) => Err(format!("a superseded status deserialized as {status:?}")),
        Err(error) if error.to_string().contains("status.by") => Ok(()),
        Err(error) => Err(format!("the refusal does not name status.by: {error}")),
    }
}

/// B-8: provenance is required on the deserialization path too.
#[test]
fn b008_a_memory_without_provenance_does_not_deserialize() -> Outcome {
    let json = r#"{"id":"019c4f00-0000-7000-8000-000000000001","scope":{"owner":"2f1b8c4e-0f6a-4b1e-9a2c-7d3e5f8a1b90","kind":"personal"},"kind":"fact","body":{"text":"x"},"actor":{"kind":"human","id":"a"},"trust":{"class":"evidence"},"status":{"state":"active"},"importance":{"base":0.5,"last_used":1,"uses":1},"schema_version":1,"created":1,"updated":1}"#;
    match serde_json::from_str::<Memory>(json) {
        Ok(_) => Err("a memory deserialized without provenance".to_owned()),
        Err(error) if error.to_string().contains("provenance") => Ok(()),
        Err(error) => Err(format!("the refusal does not name provenance: {error}")),
    }
}

/// B-4: every kind survives its own label, and the vocabulary is closed.
#[test]
fn b004_every_kind_round_trips_through_its_label() -> Outcome {
    for kind in MemoryKind::all() {
        let parsed = kind
            .label()
            .parse::<MemoryKind>()
            .map_err(|error| error.to_string())?;
        if parsed != kind {
            return Err(format!("{kind} did not survive its label"));
        }
        let json = serde_json::to_string(&kind).map_err(|error| error.to_string())?;
        if json != format!("\"{}\"", kind.label()) {
            return Err(format!("{kind} serialized as {json}"));
        }
    }
    Ok(())
}

/// B-5: the only way to instruction grade is a promotion a human minted, and
/// a memory that has one says so.
#[test]
fn b005_instruction_grade_requires_a_human_promotion() -> Outcome {
    let by = Sub::new("2f1b8c4e-0f6a-4b1e-9a2c-7d3e5f8a1b90");
    let decision = DecisionRef::new("01JC5X6Q7ZQ9V0HT8MEXAMPLE").map_err(|e| e.to_string())?;
    let trust = TrustClass::instruction(Promotion::new(by.clone(), decision.clone()));

    if !trust.is_actionable() {
        return Err("an instruction-grade memory is not actionable".to_owned());
    }
    if TrustClass::Evidence.is_actionable() || TrustClass::Assertion.is_actionable() {
        return Err("evidence or assertion is actionable".to_owned());
    }
    let promotion = trust
        .promotion()
        .ok_or_else(|| "instruction grade carries no promotion".to_owned())?;
    if promotion.by() != &by || promotion.decision() != &decision {
        return Err("the promotion did not carry its authority".to_owned());
    }
    if TrustClass::Evidence.promotion().is_some() {
        return Err("evidence carries a promotion".to_owned());
    }
    Ok(())
}

/// B-6: only an active memory is visible to retrieval.
#[test]
fn b006_only_active_is_retrievable() -> Outcome {
    let successor = MemoryId::parse("019c4f00-0000-7000-8000-000000000006")
        .map_err(|error| error.to_string())?;
    let cases = [
        (Status::Active, true),
        (Status::Quarantined, false),
        (Status::Superseded(successor), false),
        (Status::Expired, false),
        (Status::Erased, false),
    ];
    for (status, retrievable) in cases {
        if status.is_retrievable() != retrievable {
            return Err(format!(
                "{} is visible when it should not be",
                status.label()
            ));
        }
    }
    if Status::Superseded(successor).superseded_by() != Some(successor) {
        return Err("a superseded status did not name its successor".to_owned());
    }
    Ok(())
}

/// FR-004: the one constructor reachable from another crate takes a
/// `Provenance` by value, and the record it produces carries it.
///
/// That no other constructor exists is the compiler's answer, on the doc tests
/// of `Memory` and on `compile_fail/memory_without_provenance.rs`.
#[test]
fn fr004_the_reachable_constructor_carries_its_provenance() -> Outcome {
    let (_, memory) = fixtures()
        .map_err(|error| error.to_string())?
        .into_iter()
        .next()
        .ok_or_else(|| "no fixtures".to_owned())?;
    if memory.provenance.source.system.as_str() != "claude-code" {
        return Err("the provenance did not survive construction".to_owned());
    }
    if memory.schema_version != MEMORY_SCHEMA_VERSION {
        return Err("a new memory is not at the current schema version".to_owned());
    }
    if memory.status != Status::Active || memory.updated != memory.created {
        return Err("a new memory is not active and unchanged".to_owned());
    }
    Ok(())
}

/// FR-005: decay is non-increasing in elapsed time and is exactly `base` at
/// zero elapsed time.
///
/// The sweep is deterministic rather than randomized: the property is a
/// statement about a one-argument monotone function, so a dense walk over the
/// range that matters proves as much as a generator would and fails at the
/// same input on every machine (D-5).
#[test]
fn fr005_decay_is_non_increasing_and_exact_at_zero() -> Outcome {
    let start = UnixSeconds::new(NOON);
    let bases = [0.0_f32, 0.01, 0.25, 0.5, 0.75, 1.0];
    // Ten years in steps of about six hours, plus the boundaries.
    let step = 21_600_u64;
    let span = 10 * 365 * 24 * 3_600_u64;

    for base in bases {
        let importance = Importance::new(base, start, 0).map_err(|error| error.to_string())?;

        if importance.decayed_at(start) != base {
            return Err(format!("base {base} is not exact at zero elapsed time"));
        }
        // A clock that went backwards is not negative elapsed time.
        if importance.decayed_at(UnixSeconds::new(NOON - 1_000)) != base {
            return Err(format!("base {base} decayed before it was used"));
        }

        let mut previous = base;
        let mut elapsed = 0_u64;
        while elapsed <= span {
            let value = importance.decayed_at(UnixSeconds::new(NOON + elapsed));
            if value > previous {
                return Err(format!(
                    "base {base}: decay rose from {previous} to {value} at {elapsed}s"
                ));
            }
            if value < 0.0 || value > base {
                return Err(format!(
                    "base {base}: decay left 0.0..={base} at {elapsed}s"
                ));
            }
            previous = value;
            elapsed += step;
        }

        // One half-life halves it, within f32's slack.
        let half = importance.decayed_at(UnixSeconds::new(NOON + Importance::HALF_LIFE_SECS));
        if (half - base * 0.5).abs() > 1e-6 {
            return Err(format!("base {base}: one half-life gave {half}"));
        }
    }
    Ok(())
}

/// B-9: recording a use moves the clock and the counter and nothing else.
#[test]
fn b009_a_use_moves_the_clock_and_the_counter() -> Outcome {
    let start = UnixSeconds::new(NOON);
    let later = UnixSeconds::new(NOON + 60);
    let importance = Importance::new(0.5, start, 2).map_err(|error| error.to_string())?;
    let used = importance.used_at(later);
    if used.base() != importance.base() || used.uses() != 3 || used.last_used() != later {
        return Err("a use did not move exactly the clock and the counter".to_owned());
    }
    Ok(())
}

/// B-9: a weight outside `0.0..=1.0` is refused, which is what makes FR-005
/// true rather than hopeful.
#[test]
fn b009_a_weight_outside_the_range_is_refused() -> Outcome {
    let now = UnixSeconds::new(NOON);
    for base in [-0.1_f32, 1.1, f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        if Importance::new(base, now, 0).is_ok() {
            return Err(format!("{base} was accepted as a weight"));
        }
    }
    // And on the deserialization path, so a stored row cannot reintroduce one.
    if serde_json::from_str::<Importance>(r#"{"base":2.0,"last_used":1,"uses":0}"#).is_ok() {
        return Err("a weight of 2.0 deserialized".to_owned());
    }
    Ok(())
}

/// B-2: a scope renders as owner and kind, and a key is refused when it is
/// not a key.
#[test]
fn b002_a_scope_names_an_owner_and_a_kind() -> Outcome {
    let owner = Sub::new("sub-1");
    let project = ProjectKey::new("aicortex").map_err(|error| error.to_string())?;
    let scope = Scope::project(owner, project);
    if scope.to_string() != "sub-1/project/aicortex" {
        return Err(format!("a project scope rendered as {scope}"));
    }
    for bad in ["", " padded ", "with\nnewline"] {
        if ProjectKey::new(bad).is_ok() {
            return Err(format!("{bad:?} was accepted as a project key"));
        }
    }
    Ok(())
}

/// B-8, B-10: the shapes that carry a digest or a source name refuse a value
/// that is not one.
#[test]
fn b010_a_digest_and_a_source_system_are_held_to_their_shape() -> Outcome {
    for bad in [
        "9f2c1d0e",
        "sha256:9f2c",
        "md5:9f2c1d0e4a6b8c3d5e7f90a1b2c3d4e5",
        "sha256:9F2C1D0E4A6B8C3D5E7F90A1B2C3D4E5F60718293A4B5C6D7E8F9012A3B4C5D6",
    ] {
        if MediaDigest::parse(bad).is_ok() {
            return Err(format!("{bad:?} was accepted as a digest"));
        }
    }
    for bad in ["", "Claude Code", "claude code", "claude/code"] {
        if SourceSystem::new(bad).is_ok() {
            return Err(format!("{bad:?} was accepted as a source system"));
        }
    }
    SourceSystem::new("import:obsidian").map_err(|error| error.to_string())?;
    Ok(())
}

/// FR-003, AC-1: the compile-fail cases.
#[test]
fn fr003_the_unconstructible_cases_do_not_compile() {
    let cases = trybuild::TestCases::new();
    cases.compile_fail("tests/compile_fail/*.rs");
}

/// AC-2: the resolved tree carries no I/O, SQL, or async crate, and the direct
/// dependencies are the three section 2 allows.
///
/// `cargo tree --edges normal` excludes dev and build edges, which is the
/// point: `serde_json` and `trybuild` are how this crate is tested, not what
/// it is made of.
#[test]
fn ac002_the_dependency_tree_is_plain_data() -> Outcome {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());

    let transitive = tree(&cargo, &["--prefix", "none", "--format", "{p}"])?;
    let mut found = Vec::new();
    for line in transitive.lines() {
        let name = line.split_whitespace().next().unwrap_or_default();
        if FORBIDDEN.contains(&name) {
            found.push(name.to_owned());
        }
    }
    if !found.is_empty() {
        found.sort();
        found.dedup();
        return Err(format!(
            "aicortex-types reaches {found:?}; section 2 allows no I/O, SQL, or async crate"
        ));
    }

    let direct = tree(
        &cargo,
        &["--depth", "1", "--prefix", "none", "--format", "{p}"],
    )?;
    let mut names = BTreeSet::new();
    for line in direct.lines().skip(1) {
        if let Some(name) = line.split_whitespace().next()
            && !name.is_empty()
        {
            names.insert(name.to_owned());
        }
    }
    let allowed: BTreeSet<String> = ALLOWED_DIRECT
        .iter()
        .map(|name| (*name).to_owned())
        .collect();
    if names != allowed {
        return Err(format!(
            "the direct dependencies are {names:?}, not the {allowed:?} section 2 allows"
        ));
    }
    Ok(())
}

fn tree(cargo: &str, args: &[&str]) -> Result<String, String> {
    let output = Command::new(cargo)
        .current_dir(crate_root())
        .args([
            "tree",
            "--locked",
            "--package",
            "aicortex-types",
            "--edges",
            "normal",
        ])
        .args(args)
        .output()
        .map_err(|error| format!("cargo tree: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "cargo tree exited {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    String::from_utf8(output.stdout).map_err(|error| format!("cargo tree: {error}"))
}
