#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use aicortex_claims::{
    AdmissionRef, AsOf, ClaimHistory, ClaimRecord, ProjectionPolicy, SlotState, SourceSeq, TxBound,
    TxStamp, ValidBound, ValidTime, project,
};
use aicortex_types::{
    AuthorityLevel, CivilDate, Claim, ClaimId, ClaimParts, ClaimValue, EpistemicStatus, Namespace,
    PredicateRef, ProposalId, Provenance, Scope, SourceRef, SourceSystem, Sourcing, Stance,
    SubjectKey, SubjectKind, SubjectRef, SupersessionRule, TimePoint, TimeValue, Zone, ZonedTime,
};
use rahi_types::{Sub, UnixSeconds};

#[derive(Clone)]
struct Message {
    name: &'static str,
    claims: Vec<(Claim, u64)>,
}

fn claim(id: u128, subject: &str, predicate: &str, value: &str) -> Claim {
    let predicate = PredicateRef::parse(predicate).unwrap();
    Claim::new(ClaimParts {
        id: ClaimId::from_uuid(uuid::Uuid::from_u128(id)),
        scope: Scope::personal(Sub::new("traveler")),
        subject: SubjectRef {
            namespace: Namespace::new("travel").unwrap(),
            kind: SubjectKind::new("segment").unwrap(),
            key: SubjectKey::new(subject).unwrap(),
        },
        predicate,
        value: ClaimValue::Text(value.to_owned()),
        slot: None,
        epistemic: EpistemicStatus::plain(Stance::Asserted).unwrap(),
        provenance: Provenance::captured(
            SourceRef::new(SourceSystem::new("mail").unwrap()),
            UnixSeconds::new(1),
            UnixSeconds::new(2),
        ),
    })
}

fn messages() -> Vec<Message> {
    vec![
        Message {
            name: "A",
            claims: vec![
                (
                    claim(101, "segment-1", "travel:segment.departure@1", "08:00"),
                    1,
                ),
                (
                    claim(102, "segment-2", "travel:segment.departure@1", "10:00"),
                    1,
                ),
            ],
        },
        Message {
            name: "B",
            claims: vec![(
                claim(103, "segment-2", "travel:segment.departure@1", "11:00"),
                3,
            )],
        },
        Message {
            name: "C",
            claims: vec![(claim(104, "segment-1", "travel:segment.seat@1", "14C"), 2)],
        },
    ]
}

fn build(order: [usize; 3]) -> ClaimHistory {
    let messages = messages();
    let mut history = ClaimHistory::new();
    let mut tx = 0_u64;
    for index in order {
        for (claim, source_seq) in &messages[index].claims {
            tx += 1;
            let source_time = TimePoint::exact(TimeValue::DateTime(
                ZonedTime::minute(
                    CivilDate::ymd(2026, 10, u8::try_from(*source_seq).unwrap()).unwrap(),
                    12,
                    0,
                    Zone::offset(0).unwrap(),
                )
                .unwrap(),
            ));
            history
                .append_claim(ClaimRecord {
                    claim: claim.clone(),
                    valid: ValidTime::FromSourceTime,
                    source_time: Some(source_time),
                    source_seq: Some(SourceSeq(*source_seq)),
                    tx: TxStamp {
                        seq: tx,
                        recorded_at: UnixSeconds::new(100 + tx),
                    },
                    authority: AuthorityLevel::Verified,
                    admission: AdmissionRef {
                        proposal: ProposalId::from_uuid(uuid::Uuid::from_u128(
                            1_000 + claim.id.as_uuid().as_u128(),
                        )),
                        policy_id: "admission".to_owned(),
                        policy_version: 1,
                        sourcing: Sourcing::Supplier,
                    },
                    supersession: SupersessionRule::BySourceOrder,
                    hostile_content: false,
                    origin_erased: false,
                })
                .unwrap();
        }
    }
    history
}

fn as_of_on(knowledge: u64, day: u8) -> AsOf {
    AsOf {
        knowledge: TxBound::Sequence(knowledge),
        valid: ValidBound::Instant(TimePoint::exact(TimeValue::DateTime(
            ZonedTime::minute(
                CivilDate::ymd(2026, 10, day).unwrap(),
                12,
                0,
                Zone::offset(0).unwrap(),
            )
            .unwrap(),
        ))),
    }
}

fn as_of(knowledge: u64) -> AsOf {
    as_of_on(knowledge, 4)
}

#[test]
fn all_six_delivery_orders_have_one_full_knowledge_digest() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../testdata/travel/messages.json")).unwrap();
    assert_eq!(fixture["messages"].as_array().unwrap().len(), 3);
    let orders = [
        [0, 1, 2],
        [0, 2, 1],
        [1, 0, 2],
        [1, 2, 0],
        [2, 0, 1],
        [2, 1, 0],
    ];
    let mut digests = Vec::new();
    for order in orders {
        let history = build(order);
        let view = project(&history, as_of(u64::MAX), &ProjectionPolicy::v1()).unwrap();
        digests.push(view.digest.clone());
        let values: Vec<&str> = view
            .slots
            .iter()
            .filter_map(|slot| match &slot.state {
                SlotState::Value(items) => items.first().and_then(|item| match &item.value {
                    ClaimValue::Text(value) => Some(value.as_str()),
                    _ => None,
                }),
                _ => None,
            })
            .collect();
        assert!(values.contains(&"08:00"));
        assert!(values.contains(&"11:00"));
        assert!(values.contains(&"14C"));
    }
    assert!(digests.windows(2).all(|pair| pair[0] == pair[1]));
}

#[test]
fn knowledge_and_valid_bounds_are_explicit_and_do_not_use_arrival_as_source_order() {
    let history = build([1, 2, 0]);
    let after_b = project(&history, as_of(1), &ProjectionPolicy::v1()).unwrap();
    assert_eq!(after_b.slots.len(), 1);
    let SlotState::Value(items) = &after_b.slots[0].state else {
        panic!("B missing")
    };
    assert_eq!(items[0].value, ClaimValue::Text("11:00".to_owned()));

    let monday_with_full_knowledge =
        project(&history, as_of_on(u64::MAX, 1), &ProjectionPolicy::v1()).unwrap();
    let segment_two = monday_with_full_knowledge
        .slots
        .iter()
        .find(|slot| slot.slot.subject_key == "segment-2")
        .unwrap();
    let SlotState::Value(items) = &segment_two.state else {
        panic!("Monday segment-2 value missing")
    };
    assert_eq!(items[0].value, ClaimValue::Text("10:00".to_owned()));
}

#[test]
fn generated_delivery_permutations_never_change_the_digest() {
    let orders = [
        [0, 1, 2],
        [0, 2, 1],
        [1, 0, 2],
        [1, 2, 0],
        [2, 0, 1],
        [2, 1, 0],
    ];
    for left in orders {
        for right in orders {
            let a = project(&build(left), as_of(u64::MAX), &ProjectionPolicy::v1()).unwrap();
            let b = project(&build(right), as_of(u64::MAX), &ProjectionPolicy::v1()).unwrap();
            assert_eq!(a.digest, b.digest, "delivery permutations differ");
        }
    }
}

#[test]
fn fixture_names_source_order_not_delivery_order() {
    let names: Vec<_> = messages().into_iter().map(|message| message.name).collect();
    assert_eq!(names, ["A", "B", "C"]);
}
