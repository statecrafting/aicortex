# Coordination handback template

This is an authored template, not a live report or instruction.

| Field | Value |
|---|---|
| Reply identity | Stable producer/event identity |
| In reply to | Exact case, request and revision |
| Reporter | Subject/client/run and repository |
| Inspected revision | Full commit/tree, branch and dirty state |
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
