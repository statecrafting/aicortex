//! Admission records and admission policies (spec 051 B-11, B-12).
//!
//! An admission is staged into the caller's transaction beside the claim's
//! append (052) and its outbox work, so the claim, its relations, its record
//! and its work commit together or not at all (FR-008). Staging hands back
//! the Decisions to append once the transaction commits: one per batch, and
//! one per user correction (D-4). No Decision carries a claim value.
//!
//! A policy version is registered once. Its document is configuration, not
//! memory content, so its Decision names it by digest.

use std::collections::BTreeMap;

use aicortex_gate::{
    AdmissionPolicy, AdmittedClaim, LedgerEntry, PolicyRef, TargetFacts, batch_entry,
    correction_entry, policy_entry,
};
use aicortex_types::{
    AuthorityLevel, ClaimId, ClaimRelation, Evidence, ProposalId, Scope, Sourcing,
};
use rahi_store::{Statement, StoreHandle, TxnBuilder, Value};
use rahi_types::{Error, Sub, UnixSeconds};
use serde::Deserialize;

use crate::hex_digest;
use crate::scope_repo::{ScopeId, seconds_to_sql};

const INSERT_SQL: &str = "INSERT INTO claim_admission
    (claim_id, scope_id, proposal_id, policy_id, policy_version, authority, sourcing,
     verdict, evidence, relations, corrects, conflicts, admitted_at)
    VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)";

const GET_SQL: &str = "SELECT claim_id, proposal_id, policy_id, policy_version, authority,
    sourcing, verdict, evidence, relations, corrects, conflicts, admitted_at
    FROM claim_admission WHERE scope_id = $1 AND claim_id = $2";

const INSERT_POLICY_SQL: &str = "INSERT INTO admission_policy
    (policy_id, version, digest, document, registered_by, registered_at)
    VALUES ($1, $2, $3, $4, $5, $6)";

const GET_POLICY_SQL: &str = "SELECT document FROM admission_policy
    WHERE policy_id = $1 AND version = $2";

#[derive(Debug, Deserialize)]
struct Row {
    claim_id: String,
    proposal_id: String,
    policy_id: String,
    policy_version: i64,
    authority: String,
    sourcing: String,
    verdict: String,
    evidence: String,
    relations: String,
    corrects: String,
    conflicts: String,
    admitted_at: i64,
}

#[derive(Debug, Deserialize)]
struct PolicyRow {
    document: String,
}

/// One stored admission (B-11).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdmissionRecord {
    /// The admitted claim.
    pub claim_id: ClaimId,
    /// The proposal it was admitted from.
    pub proposal_id: ProposalId,
    /// The policy version it was judged under.
    pub policy: PolicyRef,
    /// The authority assigned.
    pub authority: AuthorityLevel,
    /// Where it came from.
    pub sourcing: Sourcing,
    /// The verdict, `admit`.
    pub verdict: String,
    /// The evidence considered; an item's id is its position.
    pub evidence: Vec<Evidence>,
    /// The relations appended with it.
    pub relations: Vec<ClaimRelation>,
    /// The claims it corrects.
    pub corrects: Vec<ClaimId>,
    /// The supplier claims it is kept in conflict with.
    pub conflicts: Vec<ClaimId>,
    /// When it was admitted.
    pub admitted_at: UnixSeconds,
}

/// The Decisions an admission leads to, for the caller to append after the
/// transaction commits (D-4).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdmissionDecisions {
    /// The batch Decision.
    pub batch: LedgerEntry,
    /// One per admitted user correction.
    pub corrections: Vec<LedgerEntry>,
}

/// The admission and policy tables.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ClaimAdmissionRepo;

fn json<T: serde::Serialize>(what: &str, value: &T) -> Result<String, Error> {
    serde_json::to_string(value)
        .map_err(|error| Error::Validation(format!("{what} does not serialize: {error}")))
}

fn parse<T: for<'de> Deserialize<'de>>(what: &str, text: &str) -> Result<T, Error> {
    serde_json::from_str(text)
        .map_err(|error| Error::Integrity(format!("stored {what} does not parse: {error}")))
}

impl ClaimAdmissionRepo {
    /// Stage the admission records of one batch, the claims admitted in one
    /// transaction, and return its Decisions.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] for an empty batch, or a record that does not
    /// serialize.
    pub fn stage_admit(
        txn: &mut TxnBuilder,
        batch: &[AdmittedClaim],
        actor: &Sub,
        at: UnixSeconds,
    ) -> Result<AdmissionDecisions, Error> {
        if batch.is_empty() {
            return Err(Error::Validation(
                "an admission batch admits at least one claim".to_owned(),
            ));
        }
        let mut corrections = Vec::new();
        for admitted in batch {
            let claim = admitted.claim();
            let policy = admitted.policy();
            txn.push(Statement::with_params(
                INSERT_SQL,
                vec![
                    Value::from(claim.id.to_string()),
                    Value::from(&ScopeId::of(&claim.scope)),
                    Value::from(admitted.proposal().to_string()),
                    Value::from(policy.id.as_str()),
                    Value::Integer(i64::from(policy.version)),
                    Value::from(admitted.authority().label()),
                    Value::from(admitted.sourcing().label()),
                    Value::from("admit"),
                    Value::from(json("the evidence", &admitted.evidence())?),
                    Value::from(json("the relations", &admitted.relations())?),
                    Value::from(json("the corrected claims", &admitted.corrects())?),
                    Value::from(json("the conflicts", &admitted.conflicts())?),
                    Value::Integer(seconds_to_sql(at)),
                ],
            ));
            if admitted.is_correction() {
                corrections.push(correction_entry(admitted, actor));
            }
        }
        Ok(AdmissionDecisions {
            batch: batch_entry(batch, actor),
            corrections,
        })
    }

    /// The admission of one claim of one scope, read through the leader.
    ///
    /// # Errors
    ///
    /// The store's error, or [`Error::Integrity`] when the stored row does
    /// not parse.
    pub async fn record(
        store: &StoreHandle,
        scope: &Scope,
        claim: ClaimId,
    ) -> Result<Option<AdmissionRecord>, Error> {
        let rows: Vec<Row> = store
            .query_consistent(
                GET_SQL,
                vec![
                    Value::from(&ScopeId::of(scope)),
                    Value::from(claim.to_string()),
                ],
            )
            .await?;
        rows.into_iter().next().map(decode).transpose()
    }

    /// The facts claim admission needs about the admitted claims `ids` of
    /// one scope (the `targets` of an `aicortex_gate::ClaimContext`). A
    /// claim with no admission record is absent from the answer.
    ///
    /// # Errors
    ///
    /// As [`Self::record`].
    pub async fn targets(
        store: &StoreHandle,
        scope: &Scope,
        ids: impl IntoIterator<Item = ClaimId>,
    ) -> Result<BTreeMap<ClaimId, TargetFacts>, Error> {
        let mut targets = BTreeMap::new();
        for id in ids {
            if let Some(record) = Self::record(store, scope, id).await? {
                targets.insert(
                    id,
                    TargetFacts {
                        scope: scope.clone(),
                        authority: record.authority,
                        sourcing: record.sourcing,
                    },
                );
            }
        }
        Ok(targets)
    }

    /// Stage the registration of a policy version and return its Decision
    /// (B-12). The primary key refuses a second row for a version.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] when the document does not serialize.
    pub fn stage_policy(
        txn: &mut TxnBuilder,
        policy: &AdmissionPolicy,
        registrant: &Sub,
        at: UnixSeconds,
    ) -> Result<LedgerEntry, Error> {
        let document = json("the policy", policy)?;
        let digest = policy_digest(policy)?;
        let reference = policy.reference();
        txn.push(Statement::with_params(
            INSERT_POLICY_SQL,
            vec![
                Value::from(reference.id.as_str()),
                Value::Integer(i64::from(reference.version)),
                Value::from(digest.as_str()),
                Value::from(document),
                Value::from(registrant.as_str()),
                Value::Integer(seconds_to_sql(at)),
            ],
        ));
        Ok(policy_entry(policy, &digest, registrant))
    }

    /// One registered policy version, read through the leader.
    ///
    /// # Errors
    ///
    /// The store's error, or [`Error::Integrity`] when the stored document
    /// is not a legal policy.
    pub async fn policy(
        store: &StoreHandle,
        reference: &PolicyRef,
    ) -> Result<Option<AdmissionPolicy>, Error> {
        let rows: Vec<PolicyRow> = store
            .query_consistent(
                GET_POLICY_SQL,
                vec![
                    Value::from(reference.id.as_str()),
                    Value::Integer(i64::from(reference.version)),
                ],
            )
            .await?;
        rows.into_iter()
            .next()
            .map(|row| parse("admission policy", &row.document))
            .transpose()
    }
}

/// The document digest a policy Decision names: `sha256:` and the lowercase
/// hex of its serialized bytes.
///
/// # Errors
///
/// [`Error::Validation`] when the document does not serialize.
pub fn policy_digest(policy: &AdmissionPolicy) -> Result<String, Error> {
    Ok(format!(
        "sha256:{}",
        hex_digest(json("the policy", policy)?.as_bytes())
    ))
}

fn decode(row: Row) -> Result<AdmissionRecord, Error> {
    let label = |what: &str, error: aicortex_types::TypeError| {
        Error::Integrity(format!("stored {what} does not parse: {error}"))
    };
    Ok(AdmissionRecord {
        claim_id: ClaimId::parse(&row.claim_id).map_err(|error| label("claim id", error))?,
        proposal_id: ProposalId::parse(&row.proposal_id)
            .map_err(|error| label("proposal id", error))?,
        policy: PolicyRef {
            id: row.policy_id,
            version: u32::try_from(row.policy_version).map_err(|_| {
                Error::Integrity(format!("stored policy version {}", row.policy_version))
            })?,
        },
        authority: row
            .authority
            .parse()
            .map_err(|error| label("authority", error))?,
        sourcing: row
            .sourcing
            .parse()
            .map_err(|error| label("sourcing", error))?,
        verdict: row.verdict,
        evidence: parse("evidence", &row.evidence)?,
        relations: parse("relations", &row.relations)?,
        corrects: parse("corrected claims", &row.corrects)?,
        conflicts: parse("conflicts", &row.conflicts)?,
        admitted_at: UnixSeconds::new(
            u64::try_from(row.admitted_at).map_err(|_| {
                Error::Integrity(format!("stored admission time {}", row.admitted_at))
            })?,
        ),
    })
}
