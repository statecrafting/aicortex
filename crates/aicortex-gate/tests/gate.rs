//! The corpus, the detectors and the properties that make them reviewable
//! (spec 013 FR-001, FR-002, FR-004, FR-005, B-10).
//!
//! Every assertion here is against the shipped rule set. There is no test-only
//! gate, no relaxed threshold, and no fixture that the production code would
//! treat differently.

//!
//! A test asserts; the lints that forbid a panic in a library are what a test
//! is made of, so they are relaxed here and nowhere else.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

mod common;

use std::collections::BTreeSet;

use action_gate_core::secrets::{
    CREDENTIAL_ASSIGNMENT, HIGH_ENTROPY, JWT, PEM_RULES, PREFIX_RULES, URL_CREDENTIAL,
};
use aicortex_gate::{Gate, MediaFault, Reason, Verdict};

#[test]
fn fr001_every_fixture_yields_its_recorded_verdict_and_reason_code() {
    let corpus = common::corpus();
    let judged: Vec<&common::Fixture> = corpus
        .iter()
        .filter(|fixture| fixture.kind == "verdict")
        .collect();
    assert!(
        judged.len() >= 30,
        "the corpus loader read only {} judged fixtures, so a pass proves nothing",
        judged.len()
    );

    for fixture in judged {
        let expect = fixture.expect.as_ref().unwrap_or_else(|| {
            panic!("{}: a verdict fixture records no expectation", fixture.name)
        });
        let verdict = fixture.gate().evaluate(&fixture.candidate());
        let actual = match &verdict {
            Verdict::Admit(_) => "admit",
            Verdict::Quarantine(..) => "quarantine",
            Verdict::Refuse(_) => "refuse",
        };
        assert_eq!(
            actual, expect.verdict,
            "{}: the gate answered {verdict:?}",
            fixture.name
        );

        match (&expect.code, verdict.reason()) {
            (Some(code), Some(reason)) => assert_eq!(
                reason.code(),
                code,
                "{}: the reason code is wrong",
                fixture.name
            ),
            (Some(code), None) => panic!("{}: expected {code} and got no reason", fixture.name),
            (None, Some(reason)) => assert_eq!(
                actual, "quarantine",
                "{}: an unexpected reason {reason:?}",
                fixture.name
            ),
            (None, None) => {}
        }

        if let Some(expected) = &expect.detector {
            match verdict.reason() {
                Some(Reason::SecretDetected { detector, offset }) => {
                    assert_eq!(
                        detector.as_str(),
                        expected,
                        "{}: the wrong detector fired",
                        fixture.name
                    );
                    assert!(
                        *offset < 128 * 1024,
                        "{}: the offset is not a position in the body",
                        fixture.name
                    );
                }
                other => panic!("{}: expected a detector and got {other:?}", fixture.name),
            }
        }

        if let Some(expected) = &expect.fault {
            let fault = match verdict.reason() {
                Some(Reason::UnsupportedMedia(MediaFault::TooMany { .. })) => "too_many",
                Some(Reason::UnsupportedMedia(MediaFault::UnsupportedType { .. })) => {
                    "unsupported_type"
                }
                other => panic!("{}: expected a media fault and got {other:?}", fixture.name),
            };
            assert_eq!(fault, expected, "{}: the wrong media fault", fixture.name);
        }

        if let Some(expected) = &fixture.normalized_text {
            let admitted = verdict
                .admitted()
                .unwrap_or_else(|| panic!("{}: nothing was admitted to normalize", fixture.name));
            assert_eq!(
                &admitted.memory().body.text,
                expected,
                "{}: normalization left something else behind (B-8)",
                fixture.name
            );
        }
    }
}

#[test]
fn fr001_the_corpus_holds_a_positive_case_for_every_detector() {
    let named: BTreeSet<String> = common::corpus()
        .iter()
        .filter_map(|fixture| fixture.expect.as_ref()?.detector.clone())
        .collect();

    let mut missing = Vec::new();
    for rule in PREFIX_RULES {
        if !named.contains(rule.detector.as_str()) {
            missing.push(rule.detector.as_str());
        }
    }
    for rule in PEM_RULES {
        if !named.contains(rule.detector.as_str()) {
            missing.push(rule.detector.as_str());
        }
    }
    for detector in [URL_CREDENTIAL, JWT, HIGH_ENTROPY, CREDENTIAL_ASSIGNMENT] {
        if !named.contains(detector.as_str()) {
            missing.push(detector.as_str());
        }
    }
    assert!(
        missing.is_empty(),
        "these detectors have no positive fixture: {missing:?}"
    );
    assert!(
        named.len() >= PREFIX_RULES.len() + PEM_RULES.len() + 4,
        "the corpus names {} detectors and the tables hold more",
        named.len()
    );
}

#[test]
fn fr002_a_refusal_carries_no_part_of_the_offending_value() {
    let mut checked = 0usize;
    for fixture in common::corpus() {
        let Some(secret) = fixture.secret.clone() else {
            continue;
        };
        let candidate = fixture.candidate();
        let verdict = fixture.gate().evaluate(&candidate);
        let Verdict::Refuse(reason) = &verdict else {
            panic!("{}: a fixture with a secret was not refused", fixture.name);
        };

        // Everything a caller, a log or the ledger can see of this refusal.
        let entry = aicortex_gate::ledger_entry(
            aicortex_gate::KIND_REFUSE,
            reason,
            &candidate.parts.scope,
            &common::sub("operator"),
            &aicortex_gate::DigestRef {
                digest: "0".repeat(64),
                algorithm: "HMAC-SHA-256".to_owned(),
                key_id: "0".repeat(32),
            },
            None,
        );
        let surfaces = [
            serde_json::to_string(reason).expect("a reason serializes"),
            reason.to_string(),
            format!("{reason:?}"),
            entry.reason.clone(),
            serde_json::to_string(&entry.payload).expect("a payload serializes"),
        ];

        for surface in &surfaces {
            assert!(
                !surface.contains(&secret),
                "{}: a refusal surface carries the whole secret",
                fixture.name
            );
            // Not merely the whole value: no run of it long enough to be
            // recognisable, which is what a masked form would leave behind.
            for window in secret.as_bytes().windows(8) {
                let fragment = std::str::from_utf8(window).expect("ascii fixtures");
                assert!(
                    !surface.contains(fragment),
                    "{}: a refusal surface carries {fragment:?} of the secret:\n{surface}",
                    fixture.name
                );
            }
        }
        checked += 1;
    }
    assert!(
        checked >= 30,
        "only {checked} fixtures carried a secret to search for"
    );
}

#[test]
fn fr005_the_entropy_detector_refuses_nothing_in_the_benign_corpus() {
    let corpus = common::corpus();
    let benign: Vec<&common::Fixture> = corpus
        .iter()
        .filter(|fixture| fixture.kind == "benign-identifiers")
        .collect();
    assert_eq!(benign.len(), 1, "the benign corpus is not one file");
    let identifiers = &benign[0].identifiers;
    assert!(
        identifiers.len() >= 15,
        "the benign corpus holds only {} identifiers",
        identifiers.len()
    );

    let rules = aicortex_gate::SecretRules::default();
    let mut refused = Vec::new();
    for identifier in identifiers {
        if let Some(finding) = aicortex_gate::secrets::scan(identifier, &rules) {
            refused.push((identifier.clone(), finding.detector.as_str()));
        }
    }
    assert!(
        refused.is_empty(),
        "the shipped threshold refuses benign identifiers: {refused:?}"
    );

    // And the same identifiers inside one note, which is how they arrive.
    let note = identifiers.join(" ");
    assert!(
        aicortex_gate::secrets::scan(&note, &rules).is_none(),
        "a note of benign identifiers was refused"
    );

    // The scan is not vacuously silent: the same function refuses a credential.
    assert!(
        aicortex_gate::secrets::scan("AKIA4QD7XZLM2VNBKTRW", &rules).is_some(),
        "the scanner found nothing in a known credential, so it proved nothing above"
    );
}

#[test]
fn b10_the_same_candidate_always_yields_the_same_verdict() {
    for fixture in common::corpus() {
        if fixture.kind != "verdict" {
            continue;
        }
        let gate = fixture.gate();
        let candidate = fixture.candidate();
        let first = gate.evaluate(&candidate);
        let second = gate.evaluate(&candidate);
        assert_eq!(
            first, second,
            "{}: the gate is not a function",
            fixture.name
        );

        // And a second gate built from the same rules is the same function.
        let third = fixture.gate().evaluate(&candidate);
        assert_eq!(first, third, "{}: two gates disagree", fixture.name);
    }
}

#[test]
fn b6_a_ceiling_is_configurable_downward_only() {
    let standard = aicortex_gate::Limits::standard();
    assert!(
        standard
            .clone()
            .with_max_body_bytes(aicortex_gate::DEFAULT_MAX_BODY_BYTES + 1)
            .is_err(),
        "the body ceiling was raised"
    );
    assert!(
        standard
            .clone()
            .with_max_media_refs(aicortex_gate::DEFAULT_MAX_MEDIA_REFS + 1)
            .is_err(),
        "the media ceiling was raised"
    );
    let narrowed = standard.with_max_body_bytes(1024).expect("narrowing works");
    assert_eq!(narrowed.max_body_bytes, 1024);
    assert!(
        narrowed.with_max_body_bytes(2048).is_err(),
        "a narrowed ceiling was widened again"
    );
}

#[test]
fn b5_there_is_no_rule_set_with_no_detectors() {
    // The only constructors of a rule set carry the tables; a deployment can
    // deny more and narrow more, and cannot subtract a detector.
    let rules = aicortex_gate::RuleSet::standard();
    assert_eq!(rules.secrets.prefixes.len(), PREFIX_RULES.len());
    assert_eq!(rules.secrets.pem.len(), PEM_RULES.len());
    assert!(rules.secrets.entropy.is_some());
    assert!(rules.secrets.url_credentials && rules.secrets.json_web_tokens);
    assert!(
        rules.secrets.assignments,
        "047 B-6: the assignment rule runs"
    );
    assert_eq!(Gate::standard().rules(), &rules);
    assert_eq!(aicortex_gate::RuleSet::default().secrets, rules.secrets);
}

/// FR-004: the ways around the gate do not compile.
#[test]
fn fr004_an_insert_without_a_verdict_does_not_typecheck() {
    let cases = trybuild::TestCases::new();
    cases.compile_fail("tests/compile_fail/*.rs");
}
