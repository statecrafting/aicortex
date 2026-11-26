//! Extraction is deterministic and held to the closed vocabulary (spec 017
//! FR-001, B-5, B-8, D-2).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use aicortex_graph::{
    CONFIDENCE_FLOOR, EdgeProposal, EntityKind, Extraction, Extractor, KnownEntity,
    MentionProposal, ObjectProposal, PatternExtractor, Predicate, Rejection, validate,
};
use serde::Deserialize;

#[derive(Deserialize)]
struct Fixture {
    text: String,
    known: Vec<Known>,
    expect: Expect,
}

#[derive(Deserialize)]
struct Known {
    kind: EntityKind,
    surfaces: Vec<String>,
}

#[derive(Deserialize)]
struct Expect {
    mentions: Vec<(String, EntityKind)>,
    edges: Vec<(String, String, String)>,
    rejected: Vec<(String, String)>,
}

fn reason(rejection: Rejection) -> &'static str {
    match rejection {
        Rejection::UnknownPredicate => "unknown_predicate",
        Rejection::BelowFloor => "below_floor",
        Rejection::UnmentionedEntity => "unmentioned_entity",
    }
}

#[test]
fn fr001_fixtures_match_the_recorded_expectation_exactly() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/extraction");
    let mut names: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|e| e == "json"))
        .collect();
    names.sort();
    assert!(names.len() >= 3, "the fixtures were not found in {dir:?}");
    for path in names {
        let fixture: Fixture =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let known: Vec<KnownEntity> = fixture
            .known
            .into_iter()
            .map(|k| KnownEntity {
                kind: k.kind,
                surfaces: k.surfaces,
            })
            .collect();
        let extraction = PatternExtractor.extract(&fixture.text, &known);
        for mention in &extraction.mentions {
            assert_eq!(
                fixture.text.get(mention.start..mention.end),
                Some(mention.surface.as_str()),
                "{path:?}: a mention's range must slice its surface"
            );
        }
        let validated = validate(extraction);
        let mentions: Vec<_> = validated
            .mentions
            .iter()
            .map(|m| (m.surface.clone(), m.kind))
            .collect();
        assert_eq!(mentions, fixture.expect.mentions, "{path:?}: mentions");
        let edges: Vec<_> = validated
            .edges
            .iter()
            .map(|e| {
                let object = match &e.object {
                    ObjectProposal::Entity { surface, .. } => surface.clone(),
                    ObjectProposal::Literal(text) => text.clone(),
                };
                (e.subject.clone(), e.predicate.as_str().to_owned(), object)
            })
            .collect();
        assert_eq!(edges, fixture.expect.edges, "{path:?}: edges");
        let rejected: Vec<_> = validated
            .rejected
            .iter()
            .map(|(p, r)| (p.clone(), reason(*r).to_owned()))
            .collect();
        assert_eq!(rejected, fixture.expect.rejected, "{path:?}: rejected");
    }
}

#[test]
fn the_default_extractor_is_deterministic() {
    let text = "Dana Whitfield works on Apollo. Apollo depends on Hermes.";
    assert_eq!(
        PatternExtractor.extract(text, &[]),
        PatternExtractor.extract(text, &[])
    );
}

#[test]
fn b5_the_vocabulary_is_closed_and_round_trips() {
    assert_eq!(Predicate::ALL.len(), 12);
    for predicate in Predicate::ALL {
        assert_eq!(Predicate::parse(predicate.as_str()), Some(predicate));
    }
    assert_eq!(Predicate::parse("friends_with"), None);
    assert_eq!(Predicate::parse("Works_On"), None);
    assert_eq!(EntityKind::ALL.len(), 7);
    for kind in EntityKind::ALL {
        assert_eq!(EntityKind::parse(kind.label()), Some(kind));
    }
}

#[test]
fn b8_a_foreign_extractors_output_passes_the_same_checks() {
    let mention = |surface: &str, kind| MentionProposal {
        surface: surface.to_owned(),
        kind,
        start: 0,
        end: surface.len(),
        confidence: 900,
    };
    let edge = |predicate: &str, confidence, object: &str| EdgeProposal {
        subject: "Dana".to_owned(),
        subject_kind: EntityKind::Person,
        predicate: predicate.to_owned(),
        object: ObjectProposal::Entity {
            surface: object.to_owned(),
            kind: EntityKind::Project,
        },
        confidence,
    };
    let validated = validate(Extraction {
        mentions: vec![
            mention("Dana", EntityKind::Person),
            mention("Apollo", EntityKind::Project),
        ],
        edges: vec![
            edge("works_on", 900, "Apollo"),
            edge("befriends", 900, "Apollo"),
            edge("works_on", CONFIDENCE_FLOOR - 1, "Apollo"),
            edge("works_on", 900, "Nowhere"),
        ],
    });
    assert_eq!(validated.edges.len(), 1);
    assert_eq!(
        validated.rejected,
        vec![
            ("befriends".to_owned(), Rejection::UnknownPredicate),
            ("works_on".to_owned(), Rejection::BelowFloor),
            ("works_on".to_owned(), Rejection::UnmentionedEntity),
        ]
    );
}
