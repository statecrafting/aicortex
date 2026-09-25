//! Claim proposals (spec 051 B-2).
//!
//! A proposal is a claim some actor suggests, with its evidence. It is
//! appended here whatever the gate later answers, so a held proposal can be
//! re-judged after review and an admission can be reproduced (FR-007). No
//! projection reads this table.

use aicortex_types::{ClaimProposal, ProposalId, Scope};
use rahi_store::{Statement, StoreHandle, TxnBuilder, Value};
use rahi_types::Error;
use serde::Deserialize;

use crate::scope_repo::{ScopeId, ScopeRepo, seconds_to_sql};

const INSERT_SQL: &str = "INSERT INTO claim_proposal
    (proposal_id, scope_id, claim_id, predicate, proposer, proposed_at, document)
    VALUES ($1, $2, $3, $4, $5, $6, $7)";

const GET_SQL: &str = "SELECT document FROM claim_proposal
    WHERE scope_id = $1 AND proposal_id = $2";

#[derive(Debug, Deserialize)]
struct Row {
    document: String,
}

/// The proposal table.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ClaimProposalRepo;

impl ClaimProposalRepo {
    /// Stage the append of `proposal` into the caller's transaction.
    ///
    /// The insert is a plain `INSERT` on the proposal id: a proposal is
    /// appended once and never edited.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] when the proposal does not serialize.
    pub fn stage_append(txn: &mut TxnBuilder, proposal: &ClaimProposal) -> Result<(), Error> {
        let claim = &proposal.claim;
        let scope_id = ScopeId::of(&claim.scope);
        let document = serde_json::to_string(proposal).map_err(|error| {
            Error::Validation(format!(
                "proposal {} does not serialize: {error}",
                proposal.id
            ))
        })?;
        ScopeRepo::ensure(txn, &claim.scope, proposal.proposed_at);
        txn.push(Statement::with_params(
            INSERT_SQL,
            vec![
                Value::from(proposal.id.to_string()),
                Value::from(&scope_id),
                Value::from(claim.id.to_string()),
                Value::from(claim.predicate.to_string()),
                Value::from(proposal.proposer.to_string()),
                Value::Integer(seconds_to_sql(proposal.proposed_at)),
                Value::from(document),
            ],
        ));
        Ok(())
    }

    /// One proposal of one scope, read through the leader: a re-judgement
    /// decides on it.
    ///
    /// # Errors
    ///
    /// The store's error, or [`Error::Integrity`] when the stored document
    /// does not parse.
    pub async fn get(
        store: &StoreHandle,
        scope: &Scope,
        id: ProposalId,
    ) -> Result<Option<ClaimProposal>, Error> {
        let scope_id = ScopeId::of(scope);
        let rows: Vec<Row> = store
            .query_consistent(
                GET_SQL,
                vec![Value::from(&scope_id), Value::from(id.to_string())],
            )
            .await?;
        rows.into_iter()
            .next()
            .map(|row| {
                serde_json::from_str(&row.document).map_err(|error| {
                    Error::Integrity(format!("stored proposal {id} does not parse: {error}"))
                })
            })
            .transpose()
    }
}
