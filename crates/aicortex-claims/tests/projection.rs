#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use aicortex_claims::{
    AdmissionRef, AsOf, Certainty, ClaimHistory, ClaimRecord, HistoryRelation, ProjectionPolicy,
    Retraction, SlotState, SourceSeq, TxBound, TxStamp, ValidBound, ValidTime, project,
};
use aicortex_types::{
    Actor, ActorId, AuthorityLevel, Bound, CivilDate, Claim, ClaimId, ClaimParts, ClaimRelation,
    ClaimValue, EpistemicStatus, Interval, Namespace, PredicateRef, ProposalId, Provenance,
    RelationKind, RelationTarget, Scope, SlotKey, SourceRef, SourceSystem, Stance, SubjectKey,
    SubjectKind, SubjectRef, SupersessionRule, TimePoint, TimeValue, ValidTimeMode, Zone,
    ZonedTime,
};
use rahi_types::{Sub, UnixSeconds};

fn at(value: u64) -> UnixSeconds {
    UnixSeconds::new(value)
}

fn point(day: u8, hour: u8, zone: Zone) -> TimePoint {
    TimePoint::exact(TimeValue::DateTime(
        ZonedTime::minute(CivilDate::ymd(2026, 10, day).unwrap(), hour, 0, zone).unwrap(),
    ))
}

fn claim(id: u128, predicate: &str, subject: &str, slot: Option<&str>, value: &str) -> Claim {
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
        slot: slot.map(|value| SlotKey::new(value).unwrap()),
        epistemic: EpistemicStatus::plain(Stance::Asserted).unwrap(),
        provenance: Provenance::captured(
            SourceRef::new(SourceSystem::new("mail").unwrap()),
            at(1),
            at(2),
        ),
    })
}

fn record(
    claim: Claim,
    tx: u64,
    source_seq: Option<u64>,
    authority: AuthorityLevel,
    sourcing: aicortex_types::Sourcing,
    rule: SupersessionRule,
) -> ClaimRecord {
    ClaimRecord {
        claim,
        valid: ValidTime::Timeless,
        source_time: None,
        source_seq: source_seq.map(SourceSeq),
        tx: TxStamp {
            seq: tx,
            recorded_at: at(100 + tx),
        },
        authority,
        admission: AdmissionRef {
            proposal: ProposalId::from_uuid(uuid::Uuid::from_u128(10_000 + u128::from(tx))),
            policy_id: "claim-admission".to_owned(),
            policy_version: 1,
            sourcing,
        },
        supersession: rule,
        hostile_content: false,
        origin_erased: false,
    }
}

fn as_of() -> AsOf {
    AsOf {
        knowledge: TxBound::Sequence(u64::MAX),
        valid: ValidBound::Instant(point(3, 12, Zone::offset(0).unwrap())),
    }
}

fn projected(history: &ClaimHistory) -> aicortex_claims::CurrentView {
    project(history, as_of(), &ProjectionPolicy::v1()).unwrap()
}

#[test]
fn equal_values_corroborate_and_different_values_conflict_symmetrically() {
    let mut equal = ClaimHistory::new();
    equal
        .append_claim(record(
            claim(1, "travel:segment.departure@1", "s1", None, "10:00"),
            1,
            None,
            AuthorityLevel::Verified,
            aicortex_types::Sourcing::Supplier,
            SupersessionRule::BySourceOrder,
        ))
        .unwrap();
    equal
        .append_claim(record(
            claim(2, "travel:segment.departure@1", "s1", None, "10:00"),
            2,
            None,
            AuthorityLevel::Verified,
            aicortex_types::Sourcing::Supplier,
            SupersessionRule::BySourceOrder,
        ))
        .unwrap();
    let view = projected(&equal);
    let SlotState::Value(winners) = &view.slots[0].state else {
        panic!("equal values did not corroborate")
    };
    assert_eq!(winners.len(), 2);

    let mut conflict = equal;
    conflict
        .append_claim(record(
            claim(3, "travel:segment.departure@1", "s1", None, "11:00"),
            3,
            None,
            AuthorityLevel::Verified,
            aicortex_types::Sourcing::Supplier,
            SupersessionRule::BySourceOrder,
        ))
        .unwrap();
    let view = projected(&conflict);
    let SlotState::Conflicted(candidates) = &view.slots[0].state else {
        panic!("different tied values were selected")
    };
    assert_eq!(candidates.len(), 3);
    assert_eq!(view.conflict_proposals.len(), 2);
    assert!(
        view.conflict_proposals
            .iter()
            .all(|pair| pair.left < pair.right)
    );
}

#[test]
fn user_and_supplier_remain_conflicted_regardless_of_order_and_authority() {
    for reverse in [false, true] {
        let supplier = record(
            claim(11, "travel:segment.departure@1", "s1", None, "10:00"),
            if reverse { 2 } else { 1 },
            Some(99),
            AuthorityLevel::Verified,
            aicortex_types::Sourcing::Supplier,
            SupersessionRule::BySourceOrder,
        );
        let user = record(
            claim(12, "travel:segment.departure@1", "s1", None, "11:00"),
            if reverse { 1 } else { 2 },
            Some(1),
            AuthorityLevel::UserCorrected,
            aicortex_types::Sourcing::User,
            SupersessionRule::BySourceOrder,
        );
        let mut history = ClaimHistory::new();
        history.append_claim(supplier).unwrap();
        history.append_claim(user).unwrap();
        assert!(matches!(
            projected(&history).slots[0].state,
            SlotState::Conflicted(_)
        ));
    }
}

#[test]
fn cross_slot_is_refused_and_correction_is_append_only() {
    let first = record(
        claim(21, "travel:segment.seat@1", "s1", Some("traveler"), "14C"),
        1,
        None,
        AuthorityLevel::UserAsserted,
        aicortex_types::Sourcing::User,
        SupersessionRule::ExplicitOnly,
    );
    let second = record(
        claim(22, "travel:segment.seat@1", "s1", Some("companion"), "15C"),
        2,
        None,
        AuthorityLevel::UserCorrected,
        aicortex_types::Sourcing::User,
        SupersessionRule::ExplicitOnly,
    );
    let relation = ClaimRelation::new(
        second.claim.id,
        RelationKind::Supersedes,
        RelationTarget::Claim(first.claim.id),
        second.claim.provenance.clone(),
    )
    .unwrap();
    let mut history = ClaimHistory::new();
    history.append_claim(first.clone()).unwrap();
    history.append_claim(second).unwrap();
    assert!(
        history
            .append_relation(HistoryRelation {
                relation,
                authority: AuthorityLevel::UserCorrected,
                tx: TxStamp {
                    seq: 3,
                    recorded_at: at(103)
                }
            })
            .is_err()
    );

    let corrected = record(
        claim(23, "travel:segment.seat@1", "s1", Some("traveler"), "15A"),
        4,
        None,
        AuthorityLevel::UserCorrected,
        aicortex_types::Sourcing::User,
        SupersessionRule::ExplicitOnly,
    );
    let relation = ClaimRelation::new(
        corrected.claim.id,
        RelationKind::Supersedes,
        RelationTarget::Claim(first.claim.id),
        corrected.claim.provenance.clone(),
    )
    .unwrap();
    history.append_claim(corrected.clone()).unwrap();
    history
        .append_relation(HistoryRelation {
            relation,
            authority: AuthorityLevel::UserCorrected,
            tx: TxStamp {
                seq: 5,
                recorded_at: at(105),
            },
        })
        .unwrap();
    assert_eq!(history.claims().len(), 3);
    let view = projected(&history);
    let traveler = view
        .slots
        .iter()
        .find(|slot| {
            slot.slot
                .slot
                .as_ref()
                .is_some_and(|slot| slot.as_str() == "traveler")
        })
        .unwrap();
    let SlotState::Value(winners) = &traveler.state else {
        panic!("correction did not win")
    };
    assert_eq!(winners[0].claim_id, corrected.claim.id);
    assert_eq!(winners[0].superseded, vec![first.claim.id]);
}

#[test]
fn retraction_is_bound_by_knowledge_and_preserves_claim_bytes() {
    let original = record(
        claim(31, "travel:segment.departure@1", "s1", None, "10:00"),
        1,
        None,
        AuthorityLevel::Verified,
        aicortex_types::Sourcing::Supplier,
        SupersessionRule::ExplicitOnly,
    );
    let original_bytes = serde_json::to_vec(&original.claim).unwrap();
    let mut history = ClaimHistory::new();
    history.append_claim(original.clone()).unwrap();
    history
        .append_retraction(Retraction {
            target: original.claim.id,
            reason: "supplier withdrew it".to_owned(),
            by: Actor::human(ActorId::new("traveler").unwrap()),
            provenance: original.claim.provenance.clone(),
            tx: TxStamp {
                seq: 2,
                recorded_at: at(102),
            },
        })
        .unwrap();
    let early = project(
        &history,
        AsOf {
            knowledge: TxBound::Sequence(1),
            valid: as_of().valid,
        },
        &ProjectionPolicy::v1(),
    )
    .unwrap();
    assert!(matches!(early.slots[0].state, SlotState::Value(_)));
    assert!(matches!(
        projected(&history).slots[0].state,
        SlotState::Retracted
    ));
    assert_eq!(
        serde_json::to_vec(&history.claims()[&original.claim.id].claim).unwrap(),
        original_bytes
    );
}

#[test]
fn floating_and_day_precision_report_possible_and_definite() {
    let mut floating = record(
        claim(41, "travel:segment.departure@1", "s1", None, "floating"),
        1,
        None,
        AuthorityLevel::Verified,
        aicortex_types::Sourcing::Supplier,
        SupersessionRule::ExplicitOnly,
    );
    floating.valid = ValidTime::Explicit(Box::new(
        Interval::new(
            Bound::Inclusive(point(3, 8, Zone::Floating)),
            Bound::Exclusive(point(3, 10, Zone::Floating)),
        )
        .unwrap(),
    ));
    let mut history = ClaimHistory::new();
    history.append_claim(floating).unwrap();
    let view = projected(&history);
    let SlotState::Value(candidates) = &view.slots[0].state else {
        panic!("floating value absent")
    };
    assert_eq!(candidates[0].certainty, Certainty::Possible);

    let mut day = record(
        claim(42, "travel:segment.departure@1", "s2", None, "day"),
        2,
        None,
        AuthorityLevel::Verified,
        aicortex_types::Sourcing::Supplier,
        SupersessionRule::ExplicitOnly,
    );
    day.valid = ValidTime::Explicit(Box::new(
        Interval::new(
            Bound::Inclusive(TimePoint::exact(TimeValue::Date(
                CivilDate::ymd(2026, 10, 3).unwrap(),
            ))),
            Bound::Exclusive(TimePoint::exact(TimeValue::Date(
                CivilDate::ymd(2026, 10, 4).unwrap(),
            ))),
        )
        .unwrap(),
    ));
    history.append_claim(day).unwrap();
    let view = projected(&history);
    let definite = view
        .slots
        .iter()
        .find(|slot| slot.slot.subject_key == "s2")
        .unwrap();
    let SlotState::Value(candidates) = &definite.state else {
        panic!("day value absent")
    };
    assert_eq!(candidates[0].certainty, Certainty::Definite);
}

#[test]
fn policy_versions_change_identity_and_repeated_views_are_byte_identical() {
    let mut history = ClaimHistory::new();
    history
        .append_claim(record(
            claim(51, "travel:segment.departure@1", "s1", None, "10:00"),
            1,
            None,
            AuthorityLevel::Verified,
            aicortex_types::Sourcing::Supplier,
            SupersessionRule::ExplicitOnly,
        ))
        .unwrap();
    let first_history = history.clone();
    let second_history = history.clone();
    let first = std::thread::spawn(move || projected(&first_history))
        .join()
        .unwrap();
    let second = std::thread::spawn(move || projected(&second_history))
        .join()
        .unwrap();
    assert_eq!(
        serde_json::to_vec(&first).unwrap(),
        serde_json::to_vec(&second).unwrap()
    );
    assert_eq!(first.digest, second.digest);
    let mut policy = ProjectionPolicy::v1();
    policy.version = "2".to_owned();
    policy.floating_window_seconds += 1;
    let changed = project(&history, as_of(), &policy).unwrap();
    assert_ne!(first.digest, changed.digest);
    assert_ne!(first.policy_digest, changed.policy_digest);
}

#[test]
fn valid_time_mode_validation_refuses_missing_explicit_or_source_time() {
    let record = record(
        claim(61, "travel:segment.departure@1", "s1", None, "10:00"),
        1,
        None,
        AuthorityLevel::Verified,
        aicortex_types::Sourcing::Supplier,
        SupersessionRule::ExplicitOnly,
    );
    assert!(record.validate_time(ValidTimeMode::Explicit).is_err());
    let mut sourced = record;
    sourced.valid = ValidTime::FromSourceTime;
    assert!(
        sourced
            .validate_time(ValidTimeMode::FromSourceTime)
            .is_err()
    );
}
