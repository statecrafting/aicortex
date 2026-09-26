//! Deterministic, compute-on-read projection of append-only claim history.

use core::cmp::Ordering;
use core::fmt;
use std::collections::{BTreeMap, BTreeSet};

use aicortex_types::{
    AuthorityLevel, Bound, ClaimId, ClaimValue, Precision, RelationKind, RelationTarget, Sourcing,
    SupersessionRule, TimePoint, TimeValue, Zone,
};
use chrono::{Duration, LocalResult, NaiveDate, NaiveDateTime, TimeZone};
use serde::{Deserialize, Serialize};

use crate::asof::{AsOf, TxBound, ValidBound};
use crate::history::{ClaimHistory, ClaimRecord, SlotId, ValidTime};
use crate::policy::{ProjectionPolicy, SUPPORTED_TZDB_VERSION};

/// How confidently a claim covers the requested valid-time bound.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Certainty {
    /// Every interpretation covers the bound.
    Definite,
    /// At least one widened interpretation covers the bound.
    Possible,
}

/// A candidate retained in a value or conflict state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectionCandidate {
    /// Claim id.
    pub claim_id: ClaimId,
    /// Typed value.
    pub value: ClaimValue,
    /// Authority assigned by admission.
    pub authority: AuthorityLevel,
    /// Sourcing class used for the user-versus-supplier rule.
    pub sourcing: Sourcing,
    /// Valid-time coverage.
    pub certainty: Certainty,
    /// Claims this candidate superseded in the requested window.
    pub superseded: Vec<ClaimId>,
    /// Complete provenance retained for egress.
    pub provenance: aicortex_types::Provenance,
    /// Classification retained so egress can delimit hostile content.
    pub hostile_content: bool,
    /// The cited source was erased without cascading the claim.
    pub origin_erased: bool,
}

/// Projected state of one slot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", content = "candidates", rename_all = "snake_case")]
pub enum SlotState {
    /// One value, possibly corroborated by several equal claims.
    Value(Vec<ProjectionCandidate>),
    /// Different tied values. No candidate is silently selected.
    Conflicted(Vec<ProjectionCandidate>),
    /// Claims existed by the bound, but every one was retracted.
    Retracted,
    /// No claim covers the requested valid-time bound.
    Absent,
}

/// One slot in canonical identity order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SlotView {
    /// Scope, subject, predicate, and slot identity.
    pub slot: SlotId,
    /// Projected state.
    pub state: SlotState,
}

/// Symmetric contradiction proposal. `left` is always less than `right`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConflictProposal {
    /// First claim in canonical id order.
    pub left: ClaimId,
    /// Second claim in canonical id order.
    pub right: ClaimId,
}

impl ConflictProposal {
    fn new(a: ClaimId, b: ClaimId) -> Self {
        if a <= b {
            Self { left: a, right: b }
        } else {
            Self { left: b, right: a }
        }
    }
}

/// Reproducible projection result.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CurrentView {
    /// Explicit bounds used to compute the view.
    pub as_of: AsOf,
    /// Highest transaction sequence in the supplied history.
    pub history_high_water: u64,
    /// Policy version.
    pub policy_version: String,
    /// Digest of the complete policy document.
    pub policy_digest: String,
    /// Slots in canonical order.
    pub slots: Vec<SlotView>,
    /// Proposed contradictions in canonical pair order.
    pub conflict_proposals: Vec<ConflictProposal>,
    /// Digest of every preceding field.
    pub digest: String,
}

/// Projection refusal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProjectionError {
    /// The policy asks for a time-zone database this build does not contain.
    UnsupportedTimeZoneDatabase(String),
    /// A named zone does not exist in the pinned database.
    UnknownTimeZone(String),
    /// A civil value cannot be represented.
    InvalidTime(String),
    /// The result could not be serialized for its digest.
    Serialization(String),
}

impl fmt::Display for ProjectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedTimeZoneDatabase(found) => write!(
                f,
                "policy pins time-zone database {found}, build contains {SUPPORTED_TZDB_VERSION}"
            ),
            Self::UnknownTimeZone(zone) => {
                write!(f, "time zone {zone} is not in the pinned database")
            }
            Self::InvalidTime(reason) => f.write_str(reason),
            Self::Serialization(reason) => f.write_str(reason),
        }
    }
}

impl core::error::Error for ProjectionError {}

#[derive(Clone, Copy, Debug)]
struct Window {
    definite: Option<(i64, i64)>,
    possible: (i64, i64),
}

fn date_start(date: aicortex_types::CivilDate) -> Result<NaiveDate, ProjectionError> {
    let month = u32::from(date.month().unwrap_or(1));
    let day = u32::from(date.day().unwrap_or(1));
    NaiveDate::from_ymd_opt(i32::from(date.year()), month, day)
        .ok_or_else(|| ProjectionError::InvalidTime(format!("invalid civil date {date}")))
}

fn date_end(date: aicortex_types::CivilDate) -> Result<NaiveDate, ProjectionError> {
    let start = date_start(date)?;
    match date.precision() {
        Precision::Year => NaiveDate::from_ymd_opt(i32::from(date.year()) + 1, 1, 1),
        Precision::Month => {
            let month = u32::from(date.month().unwrap_or(1));
            if month == 12 {
                NaiveDate::from_ymd_opt(i32::from(date.year()) + 1, 1, 1)
            } else {
                NaiveDate::from_ymd_opt(i32::from(date.year()), month + 1, 1)
            }
        }
        Precision::Day => start.succ_opt(),
        _ => None,
    }
    .ok_or_else(|| ProjectionError::InvalidTime(format!("civil date {date} has no end")))
}

fn naive(time: &aicortex_types::ZonedTime) -> Result<NaiveDateTime, ProjectionError> {
    let date = date_start(time.date())?;
    date.and_hms_opt(
        u32::from(time.hour_value()),
        u32::from(time.minute_value().unwrap_or(0)),
        u32::from(time.second_value().unwrap_or(0)),
    )
    .ok_or_else(|| ProjectionError::InvalidTime("invalid wall-clock time".to_owned()))
}

fn precision_duration(precision: Precision) -> Duration {
    match precision {
        Precision::Hour => Duration::hours(1),
        Precision::Minute => Duration::minutes(1),
        Precision::Second => Duration::seconds(1),
        _ => Duration::seconds(1),
    }
}

fn point_window(point: &TimePoint, policy: &ProjectionPolicy) -> Result<Window, ProjectionError> {
    if let Some(uncertainty) = point.uncertainty() {
        let first = zoned_window(uncertainty.earliest(), policy)?;
        let last = zoned_window(uncertainty.latest(), policy)?;
        return Ok(Window {
            definite: None,
            possible: (
                first.possible.0.min(last.possible.0),
                first.possible.1.max(last.possible.1),
            ),
        });
    }
    match point.value() {
        TimeValue::Date(date) => {
            let start = date_start(*date)?
                .and_hms_opt(0, 0, 0)
                .ok_or_else(|| ProjectionError::InvalidTime("invalid day start".to_owned()))?
                .and_utc()
                .timestamp();
            let end = date_end(*date)?
                .and_hms_opt(0, 0, 0)
                .ok_or_else(|| ProjectionError::InvalidTime("invalid day end".to_owned()))?
                .and_utc()
                .timestamp();
            Ok(Window {
                definite: Some((start, end)),
                possible: (start, end),
            })
        }
        TimeValue::DateTime(time) => zoned_window(time, policy),
    }
}

fn zoned_window(
    time: &aicortex_types::ZonedTime,
    policy: &ProjectionPolicy,
) -> Result<Window, ProjectionError> {
    let local = naive(time)?;
    let width = precision_duration(time.precision());
    let range = match time.zone() {
        Zone::Offset(seconds) => {
            let start = local.and_utc().timestamp() - i64::from(*seconds);
            let end = (local + width).and_utc().timestamp() - i64::from(*seconds);
            return Ok(Window {
                definite: Some((start, end)),
                possible: (start, end),
            });
        }
        Zone::Floating => {
            let center = local.and_utc().timestamp();
            let widen = i64::from(policy.floating_window_seconds);
            return Ok(Window {
                definite: None,
                possible: (
                    center - widen,
                    (local + width).and_utc().timestamp() + widen,
                ),
            });
        }
        Zone::Iana(name) => {
            let zone: chrono_tz::Tz = name
                .as_str()
                .parse()
                .map_err(|_| ProjectionError::UnknownTimeZone(name.to_string()))?;
            (zone, local)
        }
    };
    let (zone, start_local) = range;
    let resolve = |value: NaiveDateTime| match zone.from_local_datetime(&value) {
        LocalResult::Single(value) => Ok((value.timestamp(), value.timestamp())),
        LocalResult::Ambiguous(a, b) => Ok((
            a.timestamp().min(b.timestamp()),
            a.timestamp().max(b.timestamp()),
        )),
        LocalResult::None => Err(ProjectionError::InvalidTime(format!(
            "{value} does not exist in {zone}"
        ))),
    };
    let start = resolve(start_local)?;
    let end = resolve(start_local + width)?;
    let possible = (start.0, end.1);
    let definite = (start.0 == start.1 && end.0 == end.1).then_some((start.0, end.0));
    Ok(Window { definite, possible })
}

fn bound_window(bound: &ValidBound, policy: &ProjectionPolicy) -> Result<Window, ProjectionError> {
    match bound {
        ValidBound::Instant(point) => point_window(point, policy),
        ValidBound::Interval(interval) => interval_window(interval.start(), interval.end(), policy),
    }
}

fn interval_window(
    start: &Bound,
    end: &Bound,
    policy: &ProjectionPolicy,
) -> Result<Window, ProjectionError> {
    const MIN: i64 = i64::MIN / 4;
    const MAX: i64 = i64::MAX / 4;
    let start_window = match start {
        Bound::Open => (Some(MIN), MIN),
        Bound::Inclusive(point) => {
            let window = point_window(point, policy)?;
            (window.definite.map(|range| range.0), window.possible.0)
        }
        Bound::Exclusive(point) => {
            let window = point_window(point, policy)?;
            (window.definite.map(|range| range.1), window.possible.1)
        }
    };
    let end_window = match end {
        Bound::Open => (Some(MAX), MAX),
        Bound::Inclusive(point) => {
            let window = point_window(point, policy)?;
            (window.definite.map(|range| range.1), window.possible.1)
        }
        Bound::Exclusive(point) => {
            let window = point_window(point, policy)?;
            (window.definite.map(|range| range.0), window.possible.1)
        }
    };
    let definite = match (start_window.0, end_window.0) {
        (Some(start), Some(end)) => Some((start, end)),
        _ => None,
    };
    Ok(Window {
        definite,
        possible: (start_window.1, end_window.1),
    })
}

fn coverage(
    record: &ClaimRecord,
    query: Window,
    policy: &ProjectionPolicy,
) -> Result<Option<Certainty>, ProjectionError> {
    const MIN: i64 = i64::MIN / 4;
    const MAX: i64 = i64::MAX / 4;
    let valid = match &record.valid {
        ValidTime::Timeless => Window {
            definite: Some((MIN, MAX)),
            possible: (MIN, MAX),
        },
        ValidTime::FromSourceTime => {
            let point = record.source_time.as_ref().ok_or_else(|| {
                ProjectionError::InvalidTime(format!(
                    "claim {} has no source time",
                    record.claim.id
                ))
            })?;
            let start = point_window(point, policy)?;
            Window {
                definite: start.definite.map(|value| (value.0, MAX)),
                possible: (start.possible.0, MAX),
            }
        }
        ValidTime::Explicit(interval) => interval_window(interval.start(), interval.end(), policy)?,
    };
    if let (Some(claim), Some(asked)) = (valid.definite, query.definite)
        && claim.0 <= asked.0
        && claim.1 >= asked.1
    {
        return Ok(Some(Certainty::Definite));
    }
    if valid.possible.0 <= query.possible.0 && valid.possible.1 >= query.possible.1 {
        return Ok(Some(Certainty::Possible));
    }
    Ok(None)
}

fn within_tx(recorded: crate::TxStamp, bound: &TxBound) -> bool {
    match bound {
        TxBound::Sequence(seq) => recorded.seq <= *seq,
        TxBound::RecordedAt(at) => recorded.recorded_at <= *at,
    }
}

fn source_order(
    left: &ClaimRecord,
    right: &ClaimRecord,
    policy: &ProjectionPolicy,
) -> Result<Ordering, ProjectionError> {
    if let (Some(left), Some(right)) = (left.source_seq, right.source_seq) {
        let order = left.cmp(&right);
        if order != Ordering::Equal {
            return Ok(order);
        }
    }
    if let (Some(left), Some(right)) = (&left.source_time, &right.source_time) {
        let left = point_window(left, policy)?.possible;
        let right = point_window(right, policy)?.possible;
        return Ok(left.cmp(&right));
    }
    Ok(Ordering::Equal)
}

fn candidate(
    record: &ClaimRecord,
    certainty: Certainty,
    superseded: Vec<ClaimId>,
) -> ProjectionCandidate {
    ProjectionCandidate {
        claim_id: record.claim.id,
        value: record.claim.value.clone(),
        authority: record.authority,
        sourcing: record.admission.sourcing,
        certainty,
        superseded,
        provenance: record.claim.provenance.clone(),
        hostile_content: record.hostile_content,
        origin_erased: record.origin_erased,
    }
}

/// Compute the current view using only explicit arguments.
///
/// # Errors
///
/// Invalid policy identity, time-zone value, civil time, or serialization.
pub fn project(
    history: &ClaimHistory,
    as_of: AsOf,
    policy: &ProjectionPolicy,
) -> Result<CurrentView, ProjectionError> {
    if policy.time_zone_database != SUPPORTED_TZDB_VERSION {
        return Err(ProjectionError::UnsupportedTimeZoneDatabase(
            policy.time_zone_database.clone(),
        ));
    }
    let query = bound_window(&as_of.valid, policy)?;
    let mut grouped: BTreeMap<SlotId, Vec<(&ClaimRecord, Certainty)>> = BTreeMap::new();
    for record in history.claims().values() {
        if !within_tx(record.tx, &as_of.knowledge) {
            continue;
        }
        let Some(certainty) = coverage(record, query, policy)? else {
            grouped.entry(record.slot_id()).or_default();
            continue;
        };
        if certainty == Certainty::Possible && !policy.possible_counts {
            grouped.entry(record.slot_id()).or_default();
            continue;
        }
        grouped
            .entry(record.slot_id())
            .or_default()
            .push((record, certainty));
    }

    let retracted: BTreeSet<ClaimId> = history
        .retractions()
        .iter()
        .filter(|item| within_tx(item.tx, &as_of.knowledge))
        .map(|item| item.target)
        .collect();
    let mut slots = Vec::new();
    let mut proposals = BTreeSet::new();
    for (slot, all) in grouped {
        let had_candidates = !all.is_empty();
        let mut active: Vec<(&ClaimRecord, Certainty)> = all
            .into_iter()
            .filter(|(record, _)| !retracted.contains(&record.claim.id))
            .collect();
        if active.is_empty() {
            slots.push(SlotView {
                slot,
                state: if had_candidates {
                    SlotState::Retracted
                } else {
                    SlotState::Absent
                },
            });
            continue;
        }

        let active_ids: BTreeSet<ClaimId> = active.iter().map(|(r, _)| r.claim.id).collect();
        let mut superseded: BTreeMap<ClaimId, BTreeSet<ClaimId>> = BTreeMap::new();
        let mut removed = BTreeSet::new();
        for relation in history.relations().iter().filter(|relation| {
            within_tx(relation.tx, &as_of.knowledge)
                && relation.relation.kind() == RelationKind::Supersedes
        }) {
            let from = relation.relation.from();
            let RelationTarget::Claim(to) = relation.relation.to() else {
                continue;
            };
            if !active_ids.contains(&from) || !active_ids.contains(&to) {
                continue;
            }
            let Some(target) = history.claims().get(&to) else {
                continue;
            };
            if relation.authority >= target.authority {
                removed.insert(to);
                superseded.entry(from).or_default().insert(to);
            }
        }
        active.retain(|(record, _)| !removed.contains(&record.claim.id));

        let user_supplier_conflict = active
            .iter()
            .any(|(record, _)| record.admission.sourcing == Sourcing::User)
            && active
                .iter()
                .any(|(record, _)| record.admission.sourcing == Sourcing::Supplier)
            && active.iter().any(|(left, _)| {
                active.iter().any(|(right, _)| {
                    left.admission.sourcing != right.admission.sourcing
                        && left.claim.value != right.claim.value
                })
            });

        if !user_supplier_conflict {
            let max_authority = active
                .iter()
                .map(|(record, _)| record.authority)
                .max()
                .unwrap_or(AuthorityLevel::Inferred);
            active.retain(|(record, _)| record.authority == max_authority);
            if active
                .first()
                .is_some_and(|(record, _)| record.supersession == SupersessionRule::BySourceOrder)
            {
                let mut dominated = BTreeSet::new();
                for (left, _) in &active {
                    for (right, _) in &active {
                        if source_order(right, left, policy)? == Ordering::Greater {
                            dominated.insert(left.claim.id);
                            superseded
                                .entry(right.claim.id)
                                .or_default()
                                .insert(left.claim.id);
                        }
                    }
                }
                active.retain(|(record, _)| !dominated.contains(&record.claim.id));
            }
        }

        active.sort_by_key(|(record, _)| record.claim.id);
        let mut candidates: Vec<ProjectionCandidate> = active
            .iter()
            .map(|(record, certainty)| {
                candidate(
                    record,
                    *certainty,
                    superseded
                        .remove(&record.claim.id)
                        .unwrap_or_default()
                        .into_iter()
                        .collect(),
                )
            })
            .collect();
        candidates.sort_by_key(|item| item.claim_id);
        let equal = candidates
            .first()
            .is_none_or(|first| candidates.iter().all(|item| item.value == first.value));
        let state = if equal {
            SlotState::Value(candidates)
        } else {
            for (index, left) in candidates.iter().enumerate() {
                for right in candidates.iter().skip(index + 1) {
                    if left.value != right.value {
                        proposals.insert(ConflictProposal::new(left.claim_id, right.claim_id));
                    }
                }
            }
            SlotState::Conflicted(candidates)
        };
        slots.push(SlotView { slot, state });
    }
    slots.sort_by(|left, right| left.slot.cmp(&right.slot));
    let conflict_proposals: Vec<_> = proposals.into_iter().collect();
    let high_water = history.high_water();
    let policy_digest = policy.digest();
    let digest_bytes = serde_json::to_vec(&(
        &as_of,
        high_water,
        &policy.version,
        &policy_digest,
        &slots,
        &conflict_proposals,
    ))
    .map_err(|error| ProjectionError::Serialization(error.to_string()))?;
    Ok(CurrentView {
        as_of,
        history_high_water: high_water,
        policy_version: policy.version.clone(),
        policy_digest,
        slots,
        conflict_proposals,
        digest: format!("blake3:{}", blake3::hash(&digest_bytes).to_hex()),
    })
}
