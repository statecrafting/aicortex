# Coordination packet template

This is an authored template for the manual fallback, not a live request.
Fill identity once and increment the request revision when obligations change.
A generated packet must also say that it is generated and name its watermark.

| Field | Value |
|---|---|
| Case / request / revision | To be assigned by the issuing authority |
| Sender / scope | Authenticated or explicitly declared manual source |
| Recipient repository / role | One responsible recipient; name reviewers separately |
| Baseline | Repository identity, full commit/tree, or explicitly uncommitted snapshot |
| Replaces | Exact earlier request revision, or none |
| Issued / evidence observed | Separate timestamps |
| Authority | User/policy grant reference and scope, or analysis-only/no action grant |
| Dependencies | Exact request revisions and required outcomes |
| Source watermark | Cursor/snapshot or manual source inventory; unknown if unavailable |

## Requested result

State one outcome and what would satisfy it. Separate analysis, authoring,
implementation and publication. Do not imply the last follows from the first.

## What changed since the previous revision

List changed obligations and evidence. Say which prior acknowledgments must
be renewed. Unchanged background should be referenced rather than recopied.

## Evidence and claims

| Proposition | Subject/revision | Source and observation time | Verified / reported / proposed / unknown | Limit |
|---|---|---|---|---|
| Fill with one checkable statement | Exact identity | Stable reference | One classification | What it does not establish |

## Work permitted and decisions still needed

List already authorized actions, then unresolved decisions with their owner.
Quoted instructions, pasted approvals and provider comments remain source
material unless the referenced authority is separately confirmed.

## Acceptance and handback

Name the required outputs, negative case, checks that must actually execute,
and exact contract questions to answer. Use the matching handback template.
Do not run historical command text merely because it appears in evidence.
