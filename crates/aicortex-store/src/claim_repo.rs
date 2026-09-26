//! Append-only, scope- and subject-bounded claim history repository.

use aicortex_claims::{
    AdmissionRef, ClaimHistory, ClaimRecord, HistoryRelation, Retraction, SourceSeq, TxBound,
    TxStamp, ValidTime,
};
use aicortex_gate::AdmittedClaim;
use aicortex_types::{
    AuthorityLevel, Claim, ClaimId, ClaimRelation, Namespace, PredicateName, PredicateRef,
    RelationTarget, Scope, Sourcing, SubjectRef, SupersessionRule, TimePoint, ValidTimeMode,
};
use rahi_store::{Envelope, Outbox, Statement, StoreHandle, TxnBuilder, Value};
use rahi_types::{Error, Sub, UnixSeconds};
use serde::Deserialize;

use crate::{AdmissionDecisions, ClaimAdmissionRepo, ScopeId, ScopeRepo};

/// Maximum rows returned by one history page.
pub const MAX_CLAIM_PAGE_ROWS: u32 = 500;

const NEXT_TX: &str = "INSERT INTO claim_tx_counter (scope_id, seq) VALUES (?1, 1)
    ON CONFLICT (scope_id) DO UPDATE SET seq = claim_tx_counter.seq + 1";
const INSERT_CLAIM: &str = "INSERT INTO claim_history
    (claim_id, scope_id, subject_namespace, subject_kind, subject_key,
     predicate_namespace, predicate_name, predicate_version, slot_key, tx_seq, recorded_at,
     valid_time, source_time, source_seq, authority, sourcing, admission, supersession,
     hostile_content, claim)
    SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, seq, ?10, ?11, ?12, ?13,
           ?14, ?15, ?16, ?17, ?18, ?19
    FROM claim_tx_counter WHERE scope_id = ?2";
const INSERT_RELATION: &str = "INSERT INTO claim_relation
    (scope_id, from_claim_id, relation_kind, to_kind, to_id, tx_seq, recorded_at,
     authority, document)
    SELECT ?1, ?2, ?3, ?4, ?5, seq, ?6, ?7, ?8
    FROM claim_tx_counter WHERE scope_id = ?1";
const INSERT_SOURCE: &str = "INSERT INTO claim_source
    (scope_id, claim_id, source_memory_id) VALUES (?1, ?2, ?3)";
const INSERT_RETRACTION: &str = "INSERT INTO claim_retraction
    (scope_id, target_claim_id, tx_seq, recorded_at, document)
    SELECT ?1, ?2, seq, ?3, ?4 FROM claim_tx_counter WHERE scope_id = ?1";

/// Inputs around one gate-admitted claim.
pub struct ClaimAppend<'a> {
    /// Gate-owned admitted value.
    pub admitted: &'a AdmittedClaim,
    /// Valid-time representation.
    pub valid: ValidTime,
    /// Source-declared time.
    pub source_time: Option<TimePoint>,
    /// Source revision.
    pub source_seq: Option<SourceSeq>,
    /// Predicate's registered valid-time mode.
    pub valid_time_mode: ValidTimeMode,
    /// Predicate's registered supersession rule.
    pub supersession: SupersessionRule,
    /// Hostile-content classification retained for egress.
    pub hostile_content: bool,
}

/// One bounded page and its next sequence cursor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClaimHistoryPage {
    /// Append-only records in the page.
    pub history: ClaimHistory,
    /// Last sequence returned when another page may follow.
    pub next_after: Option<u64>,
}

/// A live claim or a non-attributable tombstone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StoredClaim {
    /// Content remains readable.
    Live(Box<ClaimRecord>),
    /// Erasure retained identity and non-attributable metadata only.
    Tombstone {
        /// Claim id.
        claim_id: ClaimId,
        /// Predicate retained for relation resolution.
        predicate: PredicateRef,
        /// Transaction time retained.
        tx: TxStamp,
        /// Authority retained.
        authority: AuthorityLevel,
    },
}

#[derive(Debug, Deserialize)]
struct ClaimRow {
    claim_id: String,
    tx_seq: i64,
    recorded_at: i64,
    valid_time: String,
    source_time: Option<String>,
    source_seq: Option<i64>,
    authority: String,
    sourcing: String,
    admission: String,
    supersession: String,
    hostile_content: i64,
    origin_erased: i64,
    erased: i64,
    claim: Option<String>,
    predicate_namespace: String,
    predicate_name: String,
    predicate_version: i64,
}

#[derive(Debug, Deserialize)]
struct DocumentRow {
    document: Option<String>,
    tx_seq: i64,
    recorded_at: i64,
    authority: Option<String>,
}

/// Claim history tables.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ClaimRepo;

fn json<T: serde::Serialize>(what: &str, value: &T) -> Result<String, Error> {
    serde_json::to_string(value)
        .map_err(|error| Error::Validation(format!("{what} does not serialize: {error}")))
}

fn parse<T: for<'de> Deserialize<'de>>(what: &str, text: &str) -> Result<T, Error> {
    serde_json::from_str(text)
        .map_err(|error| Error::Integrity(format!("stored {what} does not parse: {error}")))
}

fn sql_time(at: UnixSeconds) -> Result<i64, Error> {
    i64::try_from(at.get()).map_err(|_| Error::Validation("time exceeds SQLite INTEGER".to_owned()))
}

fn tx(row: &ClaimRow) -> Result<TxStamp, Error> {
    Ok(TxStamp {
        seq: u64::try_from(row.tx_seq)
            .map_err(|_| Error::Integrity(format!("stored transaction sequence {}", row.tx_seq)))?,
        recorded_at: UnixSeconds::new(u64::try_from(row.recorded_at).map_err(|_| {
            Error::Integrity(format!("stored transaction time {}", row.recorded_at))
        })?),
    })
}

impl ClaimRepo {
    /// Read one claim or tombstone by id, always under its scope.
    ///
    /// # Errors
    ///
    /// Store failure or corrupt stored data.
    pub async fn get(
        store: &StoreHandle,
        scope: &Scope,
        subject: &SubjectRef,
        id: ClaimId,
    ) -> Result<Option<StoredClaim>, Error> {
        let rows: Vec<ClaimRow> = store
            .query_consistent(
                "SELECT claim_id, tx_seq, recorded_at, valid_time, source_time, source_seq,
                 authority, sourcing, admission, supersession, hostile_content, origin_erased,
                 erased, claim, predicate_namespace, predicate_name, predicate_version
                 FROM claim_history WHERE scope_id = ?1 AND subject_namespace = ?2
                 AND subject_kind = ?3 AND subject_key = ?4 AND claim_id = ?5",
                vec![
                    Value::from(&ScopeId::of(scope)),
                    Value::from(subject.namespace.as_str()),
                    Value::from(subject.kind.as_str()),
                    Value::from(subject.key.as_str()),
                    Value::from(id.to_string()),
                ],
            )
            .await?;
        rows.into_iter().next().map(decode_stored).transpose()
    }

    /// Resolve an erased relation target without recovering its erased
    /// subject key. Scope, subject namespace, and subject kind are required.
    ///
    /// # Errors
    ///
    /// Store failure or corrupt tombstone metadata.
    pub async fn resolve_tombstone(
        store: &StoreHandle,
        scope: &Scope,
        subject: &SubjectRef,
        id: ClaimId,
    ) -> Result<Option<StoredClaim>, Error> {
        let rows: Vec<ClaimRow> = store
            .query_consistent(
                "SELECT claim_id, tx_seq, recorded_at, valid_time, source_time, source_seq,
             authority, sourcing, admission, supersession, hostile_content, origin_erased,
             erased, claim, predicate_namespace, predicate_name, predicate_version
             FROM claim_history WHERE scope_id = ?1 AND subject_namespace = ?2
             AND subject_kind = ?3 AND claim_id = ?4 AND erased = 1",
                vec![
                    Value::from(&ScopeId::of(scope)),
                    Value::from(subject.namespace.as_str()),
                    Value::from(subject.kind.as_str()),
                    Value::from(id.to_string()),
                ],
            )
            .await?;
        rows.into_iter().next().map(decode_stored).transpose()
    }

    /// Stage erasure of one claim while retaining a non-attributable
    /// tombstone and resolvable relation columns. This method never commits.
    pub fn stage_erase(txn: &mut TxnBuilder, scope: &Scope, subject: &SubjectRef, id: ClaimId) {
        let scope_id = ScopeId::of(scope);
        let identity = vec![
            Value::from(&scope_id),
            Value::from(subject.namespace.as_str()),
            Value::from(subject.kind.as_str()),
            Value::from(subject.key.as_str()),
            Value::from(id.to_string()),
        ];
        txn.push(Statement::with_params(
            "UPDATE claim_history SET subject_key = '', slot_key = NULL, claim = NULL,
             hostile_content = 0, origin_erased = 1, erased = 1
             WHERE scope_id = ?1 AND subject_namespace = ?2 AND subject_kind = ?3
             AND subject_key = ?4 AND claim_id = ?5 AND erased = 0",
            identity,
        ));
        txn.push(Statement::with_params(
            "UPDATE claim_relation SET document = NULL
             WHERE scope_id = ?1 AND from_claim_id = ?2",
            vec![Value::from(&scope_id), Value::from(id.to_string())],
        ));
        txn.push(Statement::with_params(
            "UPDATE claim_admission SET evidence = '[]', relations = '[]'
             WHERE scope_id = ?1 AND claim_id = ?2",
            vec![Value::from(&scope_id), Value::from(id.to_string())],
        ));
    }

    /// Stage claims, relations, admissions, counters, and outbox work in the
    /// caller's transaction. This method never commits.
    ///
    /// # Errors
    ///
    /// Empty batch, invalid valid time, cross-slot supersession, or
    /// serialization failure.
    pub fn stage_append(
        txn: &mut TxnBuilder,
        existing: &ClaimHistory,
        batch: &[ClaimAppend<'_>],
        actor: &Sub,
        at: UnixSeconds,
        work: &Envelope,
    ) -> Result<AdmissionDecisions, Error> {
        if batch.is_empty() {
            return Err(Error::Validation(
                "a claim append batch is empty".to_owned(),
            ));
        }
        let admitted: Vec<AdmittedClaim> = batch.iter().map(|item| item.admitted.clone()).collect();
        for item in batch {
            let claim = item.admitted.claim();
            let probe = ClaimRecord {
                claim: claim.clone(),
                valid: item.valid.clone(),
                source_time: item.source_time.clone(),
                source_seq: item.source_seq,
                tx: TxStamp {
                    seq: 1,
                    recorded_at: at,
                },
                authority: item.admitted.authority(),
                admission: AdmissionRef {
                    proposal: item.admitted.proposal(),
                    policy_id: item.admitted.policy().id.clone(),
                    policy_version: item.admitted.policy().version,
                    sourcing: item.admitted.sourcing(),
                },
                supersession: item.supersession,
                hostile_content: item.hostile_content,
                origin_erased: false,
            };
            probe
                .validate_time(item.valid_time_mode)
                .map_err(|error| Error::Validation(error.to_string()))?;
            for relation in item.admitted.relations() {
                if relation.kind() != aicortex_types::RelationKind::Supersedes {
                    continue;
                }
                let RelationTarget::Claim(target) = relation.to() else {
                    continue;
                };
                let target_slot = existing
                    .claims()
                    .get(&target)
                    .map(ClaimRecord::slot_id)
                    .or_else(|| {
                        batch
                            .iter()
                            .find(|candidate| candidate.admitted.claim().id == target)
                            .map(|candidate| {
                                aicortex_claims::SlotId::of(candidate.admitted.claim())
                            })
                    });
                if target_slot.as_ref() != Some(&probe.slot_id()) {
                    return Err(Error::Validation(
                        "supersession cannot cross a scope, subject, predicate, or slot".to_owned(),
                    ));
                }
            }

            let scope_id = ScopeId::of(&claim.scope);
            ScopeRepo::ensure(txn, &claim.scope, at);
            txn.push(Statement::with_params(
                NEXT_TX,
                vec![Value::from(&scope_id)],
            ));
            let source_time = item
                .source_time
                .as_ref()
                .map(|value| json("source time", value))
                .transpose()?;
            let source_seq = item
                .source_seq
                .map(|value| {
                    i64::try_from(value.0).map_err(|_| {
                        Error::Validation("source sequence exceeds SQLite INTEGER".to_owned())
                    })
                })
                .transpose()?;
            let admission = probe.admission.clone();
            txn.push(Statement::with_params(
                INSERT_CLAIM,
                vec![
                    Value::from(claim.id.to_string()),
                    Value::from(&scope_id),
                    Value::from(claim.subject.namespace.as_str()),
                    Value::from(claim.subject.kind.as_str()),
                    Value::from(claim.subject.key.as_str()),
                    Value::from(claim.predicate.namespace.as_str()),
                    Value::from(claim.predicate.name.as_str()),
                    Value::Integer(i64::from(claim.predicate.version)),
                    claim
                        .slot
                        .as_ref()
                        .map_or(Value::Null, |value| Value::from(value.as_str())),
                    Value::Integer(sql_time(at)?),
                    Value::from(json("valid time", &item.valid)?),
                    source_time.map_or(Value::Null, Value::from),
                    source_seq.map_or(Value::Null, Value::Integer),
                    Value::from(item.admitted.authority().label()),
                    Value::from(item.admitted.sourcing().label()),
                    Value::from(json("admission reference", &admission)?),
                    Value::from(item.supersession.label()),
                    Value::Integer(i64::from(item.hostile_content)),
                    Value::from(json("claim", claim)?),
                ],
            ));
            for source in &claim.provenance.derived_from {
                txn.push(Statement::with_params(
                    INSERT_SOURCE,
                    vec![
                        Value::from(&scope_id),
                        Value::from(claim.id.to_string()),
                        Value::from(source.to_string()),
                    ],
                ));
            }
            for relation in item.admitted.relations() {
                let (kind, id) = match relation.to() {
                    RelationTarget::Claim(id) => ("claim", id.to_string()),
                    RelationTarget::Memory(id) => ("memory", id.to_string()),
                };
                txn.push(Statement::with_params(
                    INSERT_RELATION,
                    vec![
                        Value::from(&scope_id),
                        Value::from(relation.from().to_string()),
                        Value::from(relation.kind().label()),
                        Value::from(kind),
                        Value::from(id),
                        Value::Integer(sql_time(at)?),
                        Value::from(item.admitted.authority().label()),
                        Value::from(json("claim relation", relation)?),
                    ],
                ));
            }
        }
        let decisions = ClaimAdmissionRepo::stage_admit(txn, &admitted, actor, at)?;
        Outbox::stage(txn, work);
        Ok(decisions)
    }

    /// Stage a retraction in the caller's transaction.
    ///
    /// # Errors
    ///
    /// Serialization failure or a time outside SQLite's integer range.
    pub fn stage_retraction(
        txn: &mut TxnBuilder,
        scope: &Scope,
        retraction: &Retraction,
    ) -> Result<(), Error> {
        let scope_id = ScopeId::of(scope);
        txn.push(Statement::with_params(
            NEXT_TX,
            vec![Value::from(&scope_id)],
        ));
        txn.push(Statement::with_params(
            INSERT_RETRACTION,
            vec![
                Value::from(&scope_id),
                Value::from(retraction.target.to_string()),
                Value::Integer(sql_time(retraction.tx.recorded_at)?),
                Value::from(json("retraction", retraction)?),
            ],
        ));
        Ok(())
    }

    /// Read one bounded history page. Scope and subject are mandatory.
    ///
    /// # Errors
    ///
    /// Invalid limit, store failure, or corrupt stored data.
    pub async fn history(
        store: &StoreHandle,
        scope: &Scope,
        subject: &SubjectRef,
        predicate: Option<&PredicateRef>,
        bound: &TxBound,
        after: u64,
        limit: u32,
    ) -> Result<ClaimHistoryPage, Error> {
        if limit == 0 || limit > MAX_CLAIM_PAGE_ROWS {
            return Err(Error::Validation(format!(
                "claim history limit {limit} is not between 1 and {MAX_CLAIM_PAGE_ROWS}"
            )));
        }
        let scope_id = ScopeId::of(scope);
        let mut sql = String::from(
            "SELECT claim_id, tx_seq, recorded_at, valid_time, source_time, source_seq,
             authority, sourcing, admission, supersession, hostile_content, origin_erased,
             erased, claim, predicate_namespace, predicate_name, predicate_version
             FROM claim_history WHERE scope_id = ?1 AND subject_namespace = ?2
             AND subject_kind = ?3 AND subject_key = ?4 AND tx_seq > ?5",
        );
        let mut params = vec![
            Value::from(&scope_id),
            Value::from(subject.namespace.as_str()),
            Value::from(subject.kind.as_str()),
            Value::from(subject.key.as_str()),
            Value::Integer(i64::try_from(after).map_err(|_| {
                Error::Validation("history cursor exceeds SQLite INTEGER".to_owned())
            })?),
        ];
        match bound {
            TxBound::Sequence(seq) => {
                sql.push_str(" AND tx_seq <= ?6");
                params.push(Value::Integer(i64::try_from(*seq).map_err(|_| {
                    Error::Validation("transaction bound exceeds SQLite INTEGER".to_owned())
                })?));
            }
            TxBound::RecordedAt(at) => {
                sql.push_str(" AND recorded_at <= ?6");
                params.push(Value::Integer(sql_time(*at)?));
            }
        }
        if let Some(predicate) = predicate {
            sql.push_str(
                " AND predicate_namespace = ?7 AND predicate_name = ?8 AND predicate_version = ?9",
            );
            params.extend([
                Value::from(predicate.namespace.as_str()),
                Value::from(predicate.name.as_str()),
                Value::Integer(i64::from(predicate.version)),
            ]);
        }
        let limit_index = params.len() + 1;
        sql.push_str(&format!(" ORDER BY tx_seq LIMIT ?{limit_index}"));
        params.push(Value::Integer(i64::from(limit)));
        let rows: Vec<ClaimRow> = store.query_consistent(sql, params).await?;
        let mut history = ClaimHistory::new();
        for row in &rows {
            if row.erased != 0 {
                continue;
            }
            history
                .append_claim(decode_live(row)?)
                .map_err(|error| Error::Integrity(error.to_string()))?;
        }
        let claim_ids: Vec<String> = history.claims().keys().map(ToString::to_string).collect();
        if !claim_ids.is_empty() {
            let relation_rows: Vec<DocumentRow> = store
                .query_consistent(
                    "SELECT document, tx_seq, recorded_at, authority FROM claim_relation
                 WHERE scope_id = ?1 AND from_claim_id IN (
                    SELECT claim_id FROM claim_history WHERE scope_id = ?1
                    AND subject_namespace = ?2 AND subject_kind = ?3 AND subject_key = ?4
                 ) ORDER BY tx_seq, from_claim_id, to_id",
                    vec![
                        Value::from(&scope_id),
                        Value::from(subject.namespace.as_str()),
                        Value::from(subject.kind.as_str()),
                        Value::from(subject.key.as_str()),
                    ],
                )
                .await?;
            for row in relation_rows {
                let Some(document) = row.document else {
                    continue;
                };
                let relation: ClaimRelation = parse("claim relation", &document)?;
                if !history.claims().contains_key(&relation.from()) {
                    continue;
                }
                if let RelationTarget::Claim(target) = relation.to()
                    && !history.claims().contains_key(&target)
                {
                    continue;
                }
                history
                    .append_relation(HistoryRelation {
                        relation,
                        authority: row
                            .authority
                            .as_deref()
                            .unwrap_or("inferred")
                            .parse()
                            .map_err(|error| {
                                Error::Integrity(format!("stored relation authority: {error}"))
                            })?,
                        tx: row_tx(row.tx_seq, row.recorded_at)?,
                    })
                    .map_err(|error| Error::Integrity(error.to_string()))?;
            }
            let retraction_rows: Vec<DocumentRow> = store
                .query_consistent(
                    "SELECT document, tx_seq, recorded_at, NULL AS authority FROM claim_retraction
                 WHERE scope_id = ?1 AND target_claim_id IN (
                    SELECT claim_id FROM claim_history WHERE scope_id = ?1
                    AND subject_namespace = ?2 AND subject_kind = ?3 AND subject_key = ?4
                 ) ORDER BY tx_seq, target_claim_id",
                    vec![
                        Value::from(&scope_id),
                        Value::from(subject.namespace.as_str()),
                        Value::from(subject.kind.as_str()),
                        Value::from(subject.key.as_str()),
                    ],
                )
                .await?;
            for row in retraction_rows {
                let Some(document) = row.document else {
                    continue;
                };
                let mut retraction: Retraction = parse("claim retraction", &document)?;
                retraction.tx = row_tx(row.tx_seq, row.recorded_at)?;
                history
                    .append_retraction(retraction)
                    .map_err(|error| Error::Integrity(error.to_string()))?;
            }
        }
        let next_after = (rows.len() == usize::try_from(limit).unwrap_or(usize::MAX))
            .then(|| rows.last().and_then(|row| u64::try_from(row.tx_seq).ok()))
            .flatten();
        Ok(ClaimHistoryPage {
            history,
            next_after,
        })
    }
}

fn row_tx(seq: i64, at: i64) -> Result<TxStamp, Error> {
    Ok(TxStamp {
        seq: u64::try_from(seq).map_err(|_| Error::Integrity(format!("stored tx seq {seq}")))?,
        recorded_at: UnixSeconds::new(
            u64::try_from(at).map_err(|_| Error::Integrity(format!("stored tx time {at}")))?,
        ),
    })
}

fn decode_live(row: &ClaimRow) -> Result<ClaimRecord, Error> {
    let claim: Claim = parse(
        "claim",
        row.claim.as_deref().ok_or_else(|| {
            Error::Integrity(format!("live claim {} has no document", row.claim_id))
        })?,
    )?;
    let admission: AdmissionRef = parse("admission reference", &row.admission)?;
    let sourcing: Sourcing = row
        .sourcing
        .parse()
        .map_err(|error| Error::Integrity(format!("stored sourcing: {error}")))?;
    if admission.sourcing != sourcing {
        return Err(Error::Integrity(
            "stored admission sourcing disagrees".to_owned(),
        ));
    }
    Ok(ClaimRecord {
        claim,
        valid: parse("valid time", &row.valid_time)?,
        source_time: row
            .source_time
            .as_deref()
            .map(|text| parse("source time", text))
            .transpose()?,
        source_seq: row
            .source_seq
            .map(|value| u64::try_from(value).map(SourceSeq))
            .transpose()
            .map_err(|_| Error::Integrity("stored source sequence is negative".to_owned()))?,
        tx: tx(row)?,
        authority: row
            .authority
            .parse()
            .map_err(|error| Error::Integrity(format!("stored authority: {error}")))?,
        admission,
        supersession: row
            .supersession
            .parse()
            .map_err(|error| Error::Integrity(format!("stored supersession: {error}")))?,
        hostile_content: row.hostile_content != 0,
        origin_erased: row.origin_erased != 0,
    })
}

fn decode_stored(row: ClaimRow) -> Result<StoredClaim, Error> {
    if row.erased == 0 {
        return decode_live(&row).map(Box::new).map(StoredClaim::Live);
    }
    let predicate = PredicateRef {
        namespace: Namespace::new(row.predicate_namespace.clone())
            .map_err(|error| Error::Integrity(format!("stored predicate namespace: {error}")))?,
        name: PredicateName::new(row.predicate_name.clone())
            .map_err(|error| Error::Integrity(format!("stored predicate name: {error}")))?,
        version: u32::try_from(row.predicate_version).map_err(|_| {
            Error::Integrity(format!(
                "stored predicate version {}",
                row.predicate_version
            ))
        })?,
    };
    Ok(StoredClaim::Tombstone {
        claim_id: ClaimId::parse(&row.claim_id)
            .map_err(|error| Error::Integrity(format!("stored claim id: {error}")))?,
        predicate,
        tx: tx(&row)?,
        authority: row
            .authority
            .parse()
            .map_err(|error| Error::Integrity(format!("stored authority: {error}")))?,
    })
}
