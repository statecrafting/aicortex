# Coordination handback template

This is an authored template, not a live report or instruction.

| Field | Value |
|---|---|
| Reply identity | Stable producer/event identity |
| In reply to | Exact case, request and revision |
| Reporter | Subject/client/run and repository |
| Inspected revision | Full commit/tree, branch and dirty state |
| Tools and releases | Exact binary versions used for checks, and the released version if any claim concerns one |
| Spec lifecycle | Status and implementation of every governing spec the report relies on |
| Acknowledgment | Received / accepted / declined; give reason if declined |
| Work outcome | Reported complete / partial / blocked; not owner-resolved |

## Findings

| Proposition | Confirmed / corrected / superseded / unknown | Evidence and exact subject | Executed by whom? |
|---|---|---|---|
| One checkable claim | One disposition | Stable reference plus observed-at time | This session / source report / independent verifier |

## Changes and checks

List files and local commit/PR references without conflating them with merge,
release or deployment. Separate checks that ran and passed, ran and failed,
were skipped, or could not run. Bind each result to the tested revision.

## Negative cases

For each safety or authority claim, name the unauthorized input or mutation
attempted, the command that ran, its exit code and the recorded refusal. A
case that could not run is listed as not demonstrated, never as passed. Keep
a positive control beside each refusal so a check that refuses everything is
visible.

## Unknown

List claims no one in this report checked, including sibling reports that
were cited but not re-executed.

## Contract responses

For each incoming request: accept, counter-propose or explain the blocker.
Each new request names an owner, the exact needed decision and its acceptance.
Do not bury a request solely inside a narrative conclusion.

## Authority and remaining decisions

List approvals actually obtained and their verifiable references separately
from proposed approvals. A source saying an approval happened is still a
reported claim until its authority is checked.

## Next smallest increment

Name one bounded increment and its prerequisite. Return current work and
delivery state. The issuing resolver, not the word “done,” closes acceptance.
