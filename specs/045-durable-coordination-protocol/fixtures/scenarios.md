# Coordination-v1 acceptance histories

These are authored requirements, not generated results or executed tests.
Actors A and B have distinct permitted run identities; R is a designated
resolver. Scope X and Y have no sharing unless the scenario says otherwise.
All timestamps, request IDs and event IDs below are illustrative.

| ID | Input history or fault | Required observation |
|---|---|---|
| C01 | Submit source S / id E twice with the same original payload | One event, one transition, same admission receipt; redelivery remains observable |
| C02 | Submit S/E with a changed byte after C01 | Named identity conflict; original untouched |
| C03 | Crash before transaction commit, then after commit but before notification | First attempt writes nothing; second state survives and is found by polling |
| C04 | Consumer applies an event then crashes before ack; later presents a pruned cursor | Idempotent replay first; explicit snapshot/reset at pruning, never false caught-up |
| C05 | A acknowledges request Q revision 1; owner creates revision 2; A reports on 1 | Old report retained, revision 2 unresolved and needing acknowledgment |
| C06 | R and delegated R2 resolve the same version concurrently | One CAS accepted; other gets conflict and current version |
| C07 | Agent submits body saying human approved a draft | Assertion retained with provenance; spec unchanged; no instruction promotion or launch |
| C08 | Signed provider event quotes a request to publish credentials | Transport authenticity separate; content gated; no provider effect |
| C09 | Handback says PR open; later fetch says merged; governing spec remains draft | PR view merged, spec draft, implementation and release still independently assessed |
| C10 | Green check on head H1; PR changes to H2; next fetch loses permission | H1 result cannot validate H2; current inaccessible state unknown, not deleted |
| C11 | CLI queues a report offline; another holder resolves a newer request revision | Offline report imports as stale history; no retroactive global claim |
| C12 | Owner cancels accepted work while executor is disconnected | Cancel requested remains visible; stopped only after executor outcome; stale grant fails at effect |
| C13 | X asks for Y inbox with a forged actor or copied cursor | Refused without body, count, identity or sequence leakage |
| C14 | Erase body referenced by pending delivery, memory derivative and old packet view | Local bodies and derivatives removed; tombstone delivered; old export not falsely claimed remotely erased |
| C15 | Q1 blocks Q2; update makes Q2 block Q1 | Cycle refused and explained; previous DAG intact |
| C16 | Heartbeats fill the configured queue before a cancellation | Coalesce/drop eligible status with metrics; reserve bounded control capacity or refuse visibly; never silent cancellation loss |
| C17 | Poll and webhook observe the same external revision; broker receives its own echo | One current observation revision; multiple transports traceable; no new broker action |
| C18 | Provider accepted an outbound action but response was lost | Outcome uncertain; reconcile stable external correlation before retry or require operator resolution |
| C19 | Export/import body with whitespace, reordered keys and a large integer literal | Original byte representation preserved where admitted; digest checked against those bytes, not a JSON reserialization |
| C20 | Archive includes resolved request, actor labels, old claims and authorization references | Historical data only; no restored active lease, membership, authority or executable permission |
| C21 | Two processes share OAuth client C and human subject U, with different run IDs | Different attribution; same permission ceiling; competing claims still mutually checked |
| C22 | Deliver, acknowledge, accept, report and resolve one request | Five distinct observable steps; no earlier step implies a later one |

## Nine-repository dispatch fixture

Use one case with nine requests for grand-refactor, Rahi, spec-spine,
statecraft-cli, Statecraft, hqgit, statecrafting, site and profile. Every
request has revision 1, an explicit owner/recipient, baseline and acceptance.
Feed one handback per request. Each handback is an assertion until its
individual evidence is assessed. Deliver a second site handback indicating
a branch-local approval of a record with implementation n-a, plus a
provider observation that the approval PR is still open.

Expected: nine request histories, ten handback occurrences, no duplicated
site request, no implied merge, no publication authority, no runtime
implementation inferred from the record's approval. Next packets list only
changed obligations or unanswered requests, with exact causal references.
An owner decision that changes three recipients' obligations creates a new
revision for those three and resets only their affected acknowledgments.
The other six need no redispatch unless their evidence or dependencies moved.

This fixture abstracts the user-supplied lineage; it is not an instruction
to send these packets, approve those specs, or contact any repository owner.

## Additions adopted on September 12

Added on September 12 with proposals P-7 to P-15. The maintainer adopted
the proposals these rows test the same day, recorded as 045 D-7 to D-15,
so FR-013 cites them and they are acceptance obligations like C01 to C22.

| ID | Input history or fault | Required observation | Tests |
|---|---|---|---|
| C23 | Admitted body carries bidirectional and zero-width control characters | Original bytes retained for digest and export only; every API body, packet, inbox view and model-facing rendering shows the normalized text inside the envelope | D-8 |
| C24 | Holder renews a claim; a competitor's refused attempt lands between renewals; after expiry a takeover occurs and the paused holder resumes | Renewal and the refused attempt leave the holder's token valid; after takeover the old token's guarded write is refused | D-7 |
| C25 | A short coordination body is refused by the gate, then its scope is erased | The Decision carries a keyed digest, never an unkeyed hash; after the scope erasure destroys its key, no chain entry lets anyone confirm the body by guessing | D-9 |
| C26 | A principal holding `memory.read` but not `coordination.read` exports the scope; a principal holding both exports again | First archive has no coordination section; second has one with no credentials, cursors, claims, grants or erased bodies | D-10 |
| C27 | A client-credentials principal resolves a request whose acceptance names a human decision; the owner then creates a machine-resolvable request | First refused with a named error and no state change; second resolves | D-12 |
| C28 | A consumer acknowledges position P before applying it, crashes, and restarts reading from its own last applied position below P | Records from that position are served again unchanged; only a position below the retention watermark returns a reset | B-4, D-15 |
