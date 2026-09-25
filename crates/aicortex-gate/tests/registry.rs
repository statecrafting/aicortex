//! Parity with action-gate's secret detector registry (spec 047 B-3, B-5,
//! FR-001, FR-003).
//!
//! The gate's detectors are `action_gate_core::secrets`. These tests are what
//! make that consumption admissible: every golden vector the registry ships
//! holds through this crate's scan by id, every detector id 013's corpus
//! records is a registry id carried unchanged, every corpus fixture the
//! registry ported refuses at the vector's offset through the whole gate, and
//! the crate keeps no detector implementation of its own.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

mod common;

use std::collections::{BTreeMap, BTreeSet};

use action_gate_core::secrets::{self as registry, GOLDEN_VECTORS};
use aicortex_gate::{DetectorId, Reason, SecretRules};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct Vectors {
    vectors: Vec<Vector>,
}

#[derive(Debug, Deserialize)]
struct Vector {
    id: String,
    detector: Option<String>,
    offset: Option<usize>,
    text: String,
    #[serde(default)]
    source: Option<String>,
}

fn vectors() -> Vec<Vector> {
    let parsed: Vectors = serde_json::from_str(GOLDEN_VECTORS).expect("the golden vectors parse");
    assert!(
        parsed.vectors.len() >= 60,
        "the registry ships only {} vectors, so a pass proves little",
        parsed.vectors.len()
    );
    parsed.vectors
}

/// The corpus fixture a vector was ported from, by the name it kept.
fn ported_from(vector: &Vector) -> Option<&str> {
    let source = vector.source.as_deref()?;
    let path = source.strip_prefix("aicortex:crates/aicortex-gate/testdata/corpus/")?;
    let name = path.split_once(".json@").map(|(name, _)| name)?;
    (name == vector.id).then_some(name)
}

#[test]
fn b3_every_golden_vector_holds_through_this_crates_scan_by_id() {
    let rules = SecretRules::default();
    let mut ids = BTreeSet::new();
    for vector in vectors() {
        assert!(
            ids.insert(vector.id.clone()),
            "{}: a duplicate id",
            vector.id
        );
        let finding = aicortex_gate::secrets::scan(&vector.text, &rules);
        let actual = finding.map(|finding| (finding.detector.as_str().to_owned(), finding.offset));
        let expected = vector.detector.clone().zip(vector.offset);
        assert_eq!(
            actual, expected,
            "{}: the scan disagrees with the vector",
            vector.id
        );
    }
}

#[test]
fn b3_every_detector_id_maps_to_itself() {
    let ids = registry::detector_ids();
    assert!(
        ids.len() >= 35,
        "the registry reports only {} ids",
        ids.len()
    );
    for id in ids {
        assert_eq!(
            DetectorId::from_registry(id).as_str(),
            id.as_str(),
            "the mapping renamed {id:?}"
        );
    }
}

#[test]
fn b3_every_detector_the_corpus_records_is_a_registry_id() {
    let registry_ids: BTreeSet<&str> = registry::detector_ids()
        .into_iter()
        .map(registry::DetectorId::as_str)
        .collect();
    let recorded: BTreeSet<String> = common::corpus()
        .iter()
        .filter_map(|fixture| fixture.expect.as_ref()?.detector.clone())
        .collect();
    assert!(
        recorded.len() >= 35,
        "the corpus records {} detectors",
        recorded.len()
    );
    let unknown: Vec<&String> = recorded
        .iter()
        .filter(|id| !registry_ids.contains(id.as_str()))
        .collect();
    assert!(
        unknown.is_empty(),
        "the corpus records detector ids the registry does not carry: {unknown:?}"
    );
}

#[test]
fn b3_every_ported_fixture_refuses_at_the_vectors_offset_through_the_gate() {
    let corpus: BTreeMap<String, common::Fixture> = common::corpus()
        .into_iter()
        .map(|fixture| (fixture.name.clone(), fixture))
        .collect();
    let mut compared = 0usize;
    for vector in vectors() {
        let Some(name) = ported_from(&vector) else {
            continue;
        };
        let fixture = corpus
            .get(name)
            .unwrap_or_else(|| panic!("{name}: the registry ported a fixture the corpus lacks"));
        let recorded = fixture
            .expect
            .as_ref()
            .and_then(|expect| expect.detector.clone());
        assert_eq!(recorded, vector.detector, "{name}: the detectors disagree");

        // The offset is only comparable when the gate scans exactly the
        // vector's text: one body, no title, nothing repeated.
        let Some(body) = &fixture.body else { continue };
        if body.text != vector.text || body.title.is_some() || body.repeat.is_some() {
            continue;
        }
        let verdict = fixture.gate().evaluate(&fixture.candidate());
        match (verdict.reason(), &vector.detector, vector.offset) {
            (Some(Reason::SecretDetected { detector, offset }), Some(id), Some(at)) => {
                assert_eq!(detector.as_str(), id, "{name}: the wrong detector");
                assert_eq!(*offset, at, "{name}: the offset moved");
            }
            (None, None, None) => {}
            (reason, _, _) => panic!("{name}: the gate answered {reason:?}"),
        }
        compared += 1;
    }
    assert!(
        compared >= 30,
        "only {compared} ported fixtures were compared"
    );
}

#[test]
fn fr003_the_crate_keeps_no_detector_implementation() {
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut read = 0usize;
    for entry in std::fs::read_dir(&src).expect("the crate's src is readable") {
        let path = entry.expect("a directory entry").path();
        if path.extension().is_none_or(|ext| ext != "rs") {
            continue;
        }
        let text = std::fs::read_to_string(&path).expect("a source file");
        read += 1;
        for marker in [
            "PrefixRule {",
            "PemRule {",
            "PRIVATE KEY-----",
            "fn shannon_bits",
            "fn prefix_match",
            "fn json_web_token",
            "fn url_credential",
            "fn high_entropy",
            "const DELIMITERS",
            "enum Charset",
        ] {
            assert!(
                !text.contains(marker),
                "{}: holds `{marker}`, a detector of its own (047 B-5)",
                path.display()
            );
        }
    }
    assert!(read >= 5, "only {read} source files were read");
}

#[test]
fn fr003_the_rule_types_are_the_registrys() {
    // Compiles only while the gate's rule types are the registry's own.
    let rules: registry::SecretRules = aicortex_gate::RuleSet::standard().secrets;
    let entropy: Option<registry::EntropyRule> = rules.entropy;
    let _: Option<aicortex_gate::EntropyRule> = entropy;
    assert_eq!(rules, registry::SecretRules::default());
}
