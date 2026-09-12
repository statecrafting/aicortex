---
id: "045-durable-coordination-protocol"
title: "Coordinate repository work through durable requests, replies, and reconciled evidence"
status: draft
kind: "feature"
domain: "protocol"
created: "2026-09-11"
authors: ["Bartek Kus"]
implementation: pending
risk: critical
wave: 4
depends_on:
  - "035-agent-coordination"
  - "042-portability-and-migration"
establishes:
  - "docs/design/01-coordination-and-local-integration.md"
  - "crates/aicortex-types/src/coordination.rs"
  - "crates/aicortex-types/tests/coordination.rs"
  - "crates/aicortex-store/src/coordination_repo.rs"
  - "crates/aicortex-store/src/coordination_fold.rs"
  - "crates/aicortex-store/tests/coordination.rs"
  - "crates/aicortex-store/tests/coordination_recovery.rs"
  - "crates/aicortex-store/tests/coordination_erasure.rs"
  - "crates/aicortex-api/src/coordination.rs"
  - "crates/aicortex-api/tests/coordination.rs"
  - "crates/aicortex-ingest/src/sources/coordination.rs"
  - "crates/aicortex-ingest/tests/coordination.rs"
  - "crates/aicortex-ingest/tests/coordination_portable.rs"
  - "specs/045-durable-coordination-protocol/fixtures/"
extends:
  - { spec: "011-memory-model", unit: "crates/aicortex-types/src/lib.rs", nature: additive }
  - { spec: "012-store-schema-and-repositories", unit: "crates/aicortex-store/src/lib.rs", nature: additive }
  - { spec: "012-store-schema-and-repositories", unit: "crates/aicortex-store/src/migrations.rs", nature: additive }
  - { spec: "014-memory-lifecycle-and-erasure", unit: "crates/aicortex-store/src/erasure.rs", nature: additive }
  - { spec: "020-http-api-and-scopes", unit: "crates/aicortex-api/src/lib.rs", nature: additive }
  - { spec: "020-http-api-and-scopes", unit: "crates/aicortex-api/src/router.rs", nature: additive }
  - { spec: "020-http-api-and-scopes", unit: "crates/aicortex-api/src/scopes.rs", nature: additive }
  - { spec: "030-source-adapter-framework", unit: "crates/aicortex-ingest/src/lib.rs", nature: additive }
  - { spec: "042-portability-and-migration", unit: "crates/aicortex-ingest/src/portable/export.rs", nature: additive }
  - { spec: "042-portability-and-migration", unit: "crates/aicortex-ingest/src/portable/import.rs", nature: additive }
  - { spec: "042-portability-and-migration", unit: "crates/aicortex-ingest/src/portable/format.md", nature: additive }
references:
  - { unit: { kind: file, path: "specs/002-memory-thesis/spec.md" }, role: context }
  - { unit: { kind: file, path: "specs/019-untrusted-content-boundary/spec.md" }, role: constraint }
  - { unit: { kind: file, path: "specs/024-decision-and-audit-references/spec.md" }, role: constraint }
  - { unit: { kind: file, path: "specs/033-observatory-and-agent-ingestion/spec.md" }, role: context }
summary: >
  Generalize repository handoff packets and handbacks into a durable,
  scoped coordination protocol. Requests, acknowledgment, replies,
  evidence observations, explicit resolutions and corrections retain
  their identity and revision. Inbox and outbox are projections, not
  another broker or hash chain. The same hiqlite store holds records,
  recipient state and consumer cursors transactionally; chassis events
  only wake consumers. Statecraft-cli supplies local session and Git/provider
  observations and keeps execution authority. A recalled request cannot
  authorize an action, a session report cannot approve its own specification,
  and an interrupted or disconnected consumer can reconcile what it missed.
---

# 045: Durable coordination

## 1. Purpose

The September 11 Statecraft-family work used nine repository packets,
separate working sessions, returned handbacks, a central reconciliation and
revised packets. It worked, but filenames carried revision identity,
copying carried delivery, prose mixed proposals with observed facts, and
PR status could change before the handback was read. The process needed
repeated source checks and could not answer which recipient had accepted
which version, what remained unanswered, or which decision unblocked work.

Existing specs provide the pieces but not this protocol: 033 admits four
kinds of session memory; 035 has leases and short-lived working notes; 030
resumes imports; 020 streams progress. A durable unanswered contract request
must outlive 035's one-day maximum TTL without becoming an instruction a
model may obey. This feature adds structured coordination records, separate
from both working notes and semantic memory, within the same application
store. Human-readable packets are a portable view of those records.

This is a proposed implementation work order, not an implemented record.
Approval would authorize the territory above, not sibling CLI/provider work
or any product's thesis adoption. No frozen constitutional principle is
changed. The sequencing implications are explicit in section 7.

## 2. Territory

The frontmatter lists new modules and the existing seams they extend.
Types contain no I/O. The store owns the pure transition reducer, transactions,
record repository and cursors. The API exposes scoped reads and submissions.
Intake validates portable/provider-observation submissions. Erasure and
portable export include the new records. No new crate, server, store,
credential, hash chain or general-purpose task runner is introduced.

Statecraft-cli owns agent execution, its existing journal and action broker,
provider credentials/adapters and the local UI. It is an external producer
and consumer. This spec does not modify that repository. The coordination
HTTP surface is sufficient for the first CLI consumer; the seven MCP tools
of 021 remain unchanged. Model-facing packet content follows 019.

## 3. Behavior

### B-1. Five concepts remain distinct

- A **request** asks a named recipient to supply work, information or a
  decision. It has acceptance criteria but supplies no execution authority.
- An **event** records a submitted occurrence about a request or external
  subject. It is attributable data, not a command to run its text.
- An **observation** names evidence seen at a source revision and time. Its
  authenticity, subject binding and verification limits remain explicit.
- An **authorization** is a separately checked grant held by the executor or
  effect broker. Copying, recalling or acknowledging it is not issuance.
- A **view** is a reproducible projection of scoped records: inbox, outbox,
  open decisions, blockers, evidence changes or the packet for one repo.

Only an authenticated, authorized API operation can change coordination
state. Its structured fields are checked independently of any attached
natural-language body. The body is never parsed as an authority grant.

### B-2. Envelope and identity

The boundary uses a CloudEvents 1.0 JSON structured envelope, pinned to the
1.0.2 specification, with `specversion: "1.0"`. Required `source`, `id`,
`type` and versioned `dataschema` identify the occurrence and payload.
This is a transport convention, not a trust mechanism. Routing recipients
live in the coordination payload, not in CloudEvents' meaning of `subject`.

The version-1 payload has: scope ID; conversation/case ID; request ID and
request revision when applicable; event kind; producer identity/version;
authenticated actor reference; run/session instance ID when applicable;
recipient scope/repository/role references; correlation and causation
references; expected request version for state-changing operations;
optional explicit supersedes references; evidence references; and an
optional body object reference. The server binds scope and principal from
its authenticated context and refuses contradictions in submitted fields.
`observedAt` and optional source `occurredAt` are distinct from server
`recordedAt`. Times do not resolve authority conflicts.

The closed v1 kind set is `request-created`, `request-revised`,
`delivery-acknowledged`, `delivery-declined`, `work-accepted`, `work-blocked`,
`work-reported`, `request-resolved`, `request-cancelled`, `executor-stopped`,
`source-observed`, `observation-corrected`, and `content-erased`. The envelope
type is `aicortex.coordination.<kind>.v1` and must agree with the payload.
The proposed schema identifier is `urn:aicortex:coordination:1`; it is a
versioned identifier, not a claimed published endpoint. State-derived events
such as `request-resolved` are emitted only after the corresponding checked
transition. Importing an occurrence with that name records historical data
and cannot invoke the transition. Transport-delivery attempts and reading
cursors are operational rows, not repeated work-state events.

Producer names and a client/model/session ID are attribution, not a new
identity. The authenticated rauthy subject and client remain the authority
boundary. Unknown producer software may submit under valid permission;
an unknown scope or self-asserted human actor cannot acquire permission.

Event identity is `(scope, authenticated source binding, source, id)`.
A duplicate with identical original payload bytes returns its original
receipt. The same identity with different bytes is `identity-conflict`,
never an overwrite. Unknown core payload fields, kinds and schema versions
are rejected with named errors. CloudEvents extension attributes are kept
separate from the closed application schema and cannot affect authority.

Original admitted payloads are retained as bytes, with an explicit digest
algorithm and byte digest; foreign evidence is never reserialized to check
its original digest. Source authentication material and credentials are
excluded from storage. Secret-bearing input is refused under 013, not
stored to preserve a hash. Bodies are separately erasable (B-11).

### B-3. Requests, recipients and revisions

A request records owner, intended recipient roles/repositories, subject
and baseline revision, requested result, acceptance criteria, priority,
optional deadline, dependencies on exact request revisions and evidence
needed. Recipient roles resolve through an explicit scope membership map;
a repository path or a mention in text grants no access.

Its work state is one of `open`, `accepted`, `blocked`, `reported`,
`resolved`, `cancelled`, `superseded`. Delivery is a separate per-recipient
state: `pending`, `delivered`, `acknowledged`, `declined`. Storage acceptance,
transport delivery, reading and accepting work are distinct acknowledgments.

| Operation | Precondition | Result |
|---|---|---|
| submit request | authorized sender, valid recipients | open revision with pending recipients |
| acknowledge | exact delivered revision and recipient | recipient acknowledged; work not started |
| accept | eligible recipient, current revision/version | accepted; executor still needs its own grant |
| block / report | assigned recipient and current version | blocked or reported, with reasons/evidence |
| resolve | designated resolver, current revision/version, acceptance disposition | resolved or a named refusal |
| cancel | owner/delegated cancellation authority, current version | cancelled; cancellation event queued |
| revise | owner, current version | new revision; old superseded; affected acknowledgments reset |

`reported` is a recipient claim of completion; it is not `resolved`.
Resolution records accepted criteria, unresolved exceptions and the
resolver. Imported text saying "approved" changes none of these states.
Each affected recipient must acknowledge a revision whose obligations
changed. A late reply to an old revision remains visible but cannot resolve
the current one. Reopening a terminal request creates an explicit successor,
not an invisible state rewrite. A scope migration does not auto-transfer
membership or approvals.

Dependencies are typed blockers, not natural-language guesses. Resolution
of one exact dependency may make a request eligible for attention, never
authorize execution. Cycles and references to unknown dependencies are
visible, with cycles refused on a state-changing dependency update.

### B-4. Atomic storage and recoverable delivery

Store admitted event metadata, original payload/body references, the
request transition, per-recipient delivery work and the chassis notification
outbox entry in one rahi transaction. Either all are committed or none are.
Refusals cannot advance an ingestion cursor. Durable indexes cover scope,
request revision, recipient/state and per-scope sequence.

Rahi's `Outbox::stage` and `drain` publish key-only notifications, then delete
the published outbox rows. They do not carry consumer acknowledgments.
Coordination records, per-recipient delivery rows and cursors therefore
remain application tables in the same hiqlite group. No correctness
property relies on receiving every notification. A disconnected consumer
polls the durable ordered records using a scope/filter-bound cursor.

A consumer acknowledgment includes the last successfully applied position;
it cannot advance beyond delivery or past an unapplied required record.
Consumers use stable effect/request IDs and idempotent application. Crashes
before acknowledgment cause redelivery. Stale or expired cursors return an
explicit reset/snapshot requirement and a retention watermark, never an
empty "caught up" result. Snapshot plus position is read consistently.
Unauthorized scopes leak neither entries nor counts/cursor positions.

Retries use bounded exponential backoff and jitter with an attempt ceiling.
Dead deliveries remain visible and can be requeued by an authorized operator.
A maximum age never silently converts an unanswered request into success.
Queue limits, payload limits and per-scope quotas apply before admission.
Per-scope ordering describes accepted records, not global real-world time.

### B-5. Single authority, fencing and cancellation

One configured coordination authority owns transitions for a workspace.
Mutations use a compare-and-set of request version and the relevant current
claim/fencing token where a claim protects the operation. Losing a race
returns conflict with a readable current revision, not a last-write-wins
merge. Two agents sharing an OAuth client remain distinct run instances
for attribution, but receive no permissions from that distinction alone.

Aicortex's work claim protects coordination writes only. It does not fence
GitHub or an arbitrary process. The CLI executor/action broker must check
its own current grant and cancellation/lease status at the actual effect.
A cancellation request is not confirmation that execution has stopped.
That confirmation is a separate executor outcome.

Offline clients may durably queue reports in the CLI's existing local
journal/export path. They cannot claim global work or resolve a shared
request while disconnected from its authority. Replay after reconnect must
pass current authorization and revision checks. Two local aicortex cells do
not provide globally exclusive claims merely because their records sync.
Authority migration requires quiescence, a transferred cursor/snapshot and
new lease epoch; old tokens are invalid. Active-active authority is outside
this spec.

### B-6. Git and provider observations

The first integration consumes explicit, scoped observations supplied by
statecraft-cli, using a documented protocol rather than reading private
client transcripts. The existing four-kind session path of 033 stays
compatible. Coordination kinds use the new versioned endpoint rather than
silently widening its closed enum or changing 030's update semantics.

Git evidence names canonical repository identity, object algorithm, full
commit and tree IDs, path and blob ID where relevant, and observed branch
state. Uncommitted evidence names the checkout/run and is labelled mutable;
a branch name or local absolute path alone is not portable subject identity.

Provider evidence names provider host, immutable repository/resource IDs,
object kind, current resource revision or response digest, relevant PR head
and base, check-run/review identity, fetch time and authorization scope.
URLs are locators, not primary identities. An edited comment is a new
observation revision; it does not overwrite an old occurrence. Forks and
repositories with the same display name remain distinct. Token-bearing
URLs are rejected or sanitized before the producer constructs the record.

Webhooks are change hints with deduplication and transport verification,
followed by permitted authoritative fetch/reconciliation where supported.
They are not ordered truth. Polling reconciles missed notifications and
also supports a local machine without an inbound listener. ETags/cursors
and overlapping time windows may optimize reads, but cannot create a gap
at a pagination boundary. Exact provider support belongs to the CLI adapter
contract, not an assumed property of every provider.

A provider 403/404 or lost permission means inaccessible/unknown unless an
explicit deletion observation establishes deletion. PR merged, spec
approved, implementation complete, released, deployed and independently
verified remain separate propositions. A check result is bound to its
actual tested head; a changed head invalidates its use for a later head.

No inbound text, issue, PR comment or webhook causes an outbound comment,
merge, approval or agent launch. Outbound effects remain the CLI/Statecraft
broker's authorized operations. Echoed provider events retain causation and
external IDs so a broker action is not reissued from its own notification.
For ambiguous remote outcomes, reconcile before retrying; do not promise
exactly-once remote effects when the provider supplies no such primitive.

### B-7. Reconciliation is proposition-specific

A handback separates assertions, proposed decisions, requested contracts,
evidence references and unresolved issues. Each proposition names subject,
revision, observer, observation time and evidence coverage. The reconciler
compares like propositions, not documents as a whole. A newer report does
not erase an older report about a different commit.

Example: a session says a PR is open at time A; a provider fetch shows it
merged at time B. The current PR-state projection changes, retaining A as
historical. It does not mark the draft spec approved or its feature built.
Two contradictory reports about the same subject/revision remain conflict
until evidence or an authorized resolution explains the discrepancy.

Evidence assessment records integrity, signature validity, issuer trust
and subject binding separately, with a separate policy/admission result.
Each assessment includes verifier identity/version and pass/fail/unknown/
not-applicable outcomes. This is an adapter mapping to the family contract,
not a new signature implementation here. Aicortex references independent
verifier results and applies memory trust rules; it does not reimplement
spec-spine, the CLI bundle verifier or Rahi's chain verification.

### B-8. Query views and bounded attention

Expose inbox, outbox, request detail/history and changes-since-cursor as
scoped indexed API reads under `/api/v1/coordination`. Separate read/write
permissions are mapped from rauthy-issued scopes; `memory.write` alone does
not grant coordination resolution or outbound effects. The implementation
must document route DTOs and action-specific role checks before coding.

The minimum route contract is `POST /ingest` (attributed external occurrences,
including historical reports), `POST /requests` (checked creation),
`POST /requests/{id}/transitions` (checked action with expected revision and
version), `GET /inbox`, `GET /outbox`, `GET /requests/{id}`, and
`GET /changes`, relative to that prefix. Ingest returns an event receipt
and mapped disposition, never a claim that work or an external effect ran.
An external occurrence may feed reconciliation but cannot call the transition
route internally with stronger privilege. The scope permissions are
`coordination.read` and `coordination.write`; resolver/owner/recipient checks
apply in addition to token scope. Import/export still use the separate
administrative authority of 042.

Inbox groups actionable requests, changed evidence, unanswered decisions
and conflicts. Outbox shows each recipient's delivery/acknowledgment, work
state, retry status and age. Filters include repo, role, request revision,
blocking status, due state and producer. Attention is driven by explicit
subscriptions/assignments, not broadcast to every agent. Status heartbeats
may be coalesced; requests, corrections, cancellations and resolutions may
not be dropped. Notifications contain IDs and state, not private bodies.

Semantic recall may supply labelled related context. Ranking cannot hide a
required assigned request, acknowledge it, close it or decide precedence.
Consumers render deterministic action/status lists before optional recalled
context. A digest lists omitted/truncated items and its source watermark.

### B-9. Packets remain portable, versioned views

A rendered packet names case/request/revision, source scope, target repo and
role, baseline commit/tree, purpose, acceptance, dependencies, evidence,
unknowns, authorization references (if any), generated-at time and source
watermark. It explicitly labels historical quoted instructions as data.
A handback references the exact packet revision and distinguishes what was
inspected, changed, executed, skipped, reported and independently verified.

Manual Markdown handoffs remain supported without aicortex. Importing one
requires explicit source binding and preview, never ambient discovery of
all documents on a machine. A packet's text claiming human approval remains
an assertion until the approving authority is separately verified.
Git publication is selective and explicit: reviewed decisions/contracts may
be committed, but live inbox state, credentials, raw transcripts and private
provider content are not committed by default. A content change creates a
new exported revision. Generated views identify themselves as generated;
reviewed authored corpus records remain Markdown under spec-spine governance.

### B-10. Semantic memory remains a separate projection

Durable requests outlive short working-note TTLs, but their routing and
state are not embedded or curated by default. An explicit capture can turn
a resolved decision or lesson into a memory under 011/013/014/033. It retains
provenance and the original assertion grade. A summary never inherits a
human authorization merely because it quotes one. Instruction-grade promotion
still requires 023's human decision; coordination has no alternate promotion
path. A promoted memory is not an executor permit by itself.

### B-11. Retention, erasure and access revocation

Store message bodies and imported content as erasable objects in the same
store. Immutable occurrence identity does not mean immortal payload.
Erasure removes authorized body copies, derivatives, search material and
pending delivery payloads; tombstones retain only the minimal opaque IDs,
transition metadata and required decision references. Content hashes of
erased bodies are not kept by default because low-entropy content can be
guessed from a hash. Pending consumers receive a tombstone/reset rather
than the erased body. Reimport cannot resurrect an erased source item
without an explicit authorized restoration operation and new provenance.

Capture minimal duplicate-suppression identity under a documented retention
policy; where erasure removes it too, report that replay deduplication is
no longer assured and require a fresh import decision. No body, raw payload,
credential or private digest enters Rahi's immutable decision chain.
Source-access revocation suspends new delivery and invalidates cached views;
retained data follows the configured authorization/retention policy, not
an assumption of perpetual provider access. Export 042 gains a separately
versioned coordination section, never executable grants or live leases.
Import does not restore permissions, membership, resolved authority or
active work claims from an untrusted archive. It creates provenance-labelled
historical records pending reauthorization.

### B-12. Local product integration

The supported composition proposal is one Rahi-based aicortex instance per
configured local installation/workspace authority, shared by CLI and agent
clients over the existing authenticated HTTP surface. CLI can manage its
lifecycle and show its UI, but is not reimplemented inside it. No per-client
stdio server or auth-bypassing embedded database is added. Pure typed client
libraries do not instantiate another store.

Local means no mandatory Statecraft-hosted account or remote memory service.
It does not mean skipping Rauthy or inventing a static local token. Local
identity provisioning, renewal, Rahi distribution and supported OS packaging
must be proven before this composition is called installable. Aicortex being
unavailable must not erase CLI execution evidence or prevent existing local
operations unrelated to shared claims. The CLI reports coordination pending
or offline and replays through its existing durable journal seam.

## 4. Functional requirements

The following fixture IDs are acceptance obligations, not claims about
existing code. The Markdown matrix in `fixtures/scenarios.md` defines the
smallest representative histories; implementations add machine fixtures
under their owned test modules without silently changing expected outcomes.

- **FR-001.** C01/C02: duplicate deliveries and identity conflicts preserve
  one original event; modified bytes never overwrite it.
- **FR-002.** C03/C04: crash boundaries preserve atomic transition/delivery
  and replay; cursor loss returns a scoped reset, not false success.
- **FR-003.** C05/C06: late replies cannot resolve a revised request; two
  competing resolvers yield one accepted CAS and one conflict.
- **FR-004.** C07/C08: producer-authenticated claims and quoted approvals
  cannot promote memory, launch a session or authorize a provider mutation.
- **FR-005.** C09/C10: stale PR observations and head changes change only
  the propositions they establish; missing access produces unknown.
- **FR-006.** C11/C12: offline handbacks reconcile without acquiring shared
  authority; cancellation requires an executor acknowledgment to mean stopped.
- **FR-007.** C13/C14: cross-scope reads/writes and forged actor mappings
  fail without leaking counts; erasure removes pending/derived content.
- **FR-008.** C15/C16: cyclic blockers fail explicitly; high-volume heartbeats
  cannot starve control records and overflow remains observable.
- **FR-009.** C17/C18: webhook/poll duplicates and self-echoes create no
  repeated broker action; ambiguous remote outcomes remain unresolved.
- **FR-010.** C19/C20: portable round-trip preserves permitted original
  bytes and provenance; import cannot restore live claims or authority.
- **FR-011.** C21/C22: two runs under one OAuth client are attributable
  separately; transport success, acknowledgment, acceptance and resolution
  are separately rendered and queried.
- **FR-012.** A deterministic nine-repository fixture renders one packet
  per recipient, incorporates a later site update, and produces only changed
  or unanswered work on the second dispatch. No duplicate source entry and
  no historical approval may alter the current authority state.

## 5. Acceptance criteria

- **AC-1.** The named type/store/API/intake tests below execute and pass.
  Missing crates, filtered-out tests or skipped suites are not success.
- **AC-2.** A temporary single-node Rahi instance with authenticated fixture
  subjects demonstrates request, acknowledgment, report and resolution across
  two clients, a restart and a disconnected consumer. Retain exact binaries,
  fixtures and results. A stubbed identity result is labelled as such.
- **AC-3.** One statecraft-cli producer publishes a local handback and a
  Git/provider observation through the documented endpoint; its UI distinguishes
  reported from verified and refuses action without a grant. This is a sibling
  integration prerequisite, not permission to edit that repo here.
- **AC-4.** Rebuilding projections from retained events and a versioned
  checkpoint yields identical visible states at the same watermark. An expired
  retention window is explicitly reported; erased content never reappears.
- **AC-5.** The scope-bound queries use bounded pagination and indexes;
  two-scope, duplicate, out-of-order, crash, stale-fence and erasure fixtures
  run. Embedding/model availability is not a prerequisite for inbox correctness.

## 6. Out of scope

A general message broker, event-sourcing every application concern, active-active
leases, full workflow execution, hosted billing, a new identity service,
provider credential storage in aicortex, GitHub comments or PR mutation,
reading vendor-private session files, redefining the seven MCP tools,
and changing existing memory trust classes. No hqgit dependency is needed.
No production deployment, draft approval or external posting is authorized
by authoring this specification.

## 7. Resolved decisions

No owner ratification is recorded. The following are proposed design choices
from the September 11 user-requested analysis, with alternatives and impact
in `docs/design/01-coordination-and-local-integration.md`.

- **P-1.** Durable structured coordination in the existing application
  store, plus inbox/outbox projections and chassis wakeups. Reject mailbox
  folders as the primary runtime authority and an additional external broker
  for the first slice.
- **P-2.** CLI-managed local Rahi cell, not an embedded unauthenticated
  database. If a true library-only embedding is required, amend the frozen
  chassis/identity principles explicitly before designing that different product.
- **P-3.** Keep execution/effect authority in CLI/Statecraft. Aicortex is
  authoritative for its own coordination state, never for Git/provider facts
  it has not verified or for an executor's permission.
- **P-4.** Current ordinals put this extension in wave 4. This authoring
  does not reorder 002 or bypass the approved dependencies. An earlier
  coordination slice requires an explicit sequencing revision, with the
  needed types/store/gate/API/claims extracted from the linear plan. It
  does not require embeddings, curation or Kubernetes on technical grounds.
- **P-5.** Rahi 012 currently exposes notification outbox semantics rather
  than a durable task-consumer acknowledgment API. Stage durable application
  work in the same transaction and treat notifications only as hints.
  Validate the available lease API before claiming 035's requested duration
  and renewal semantics work. Chassis changes belong in Rahi.
- **P-6.** Before implementing 010, reconcile its published-only Rahi
  dependencies and crate list with the actual distribution and public APIs.
  A Git revision useful for a spike does not satisfy its no-git exact-version
  contract. This spec grants no dependency waiver.

## Verification

These are implementation acceptance, deliberately not run successfully by
this documentation-only authoring session. The corpus gate validates the
draft; it cannot validate the absent runtime.

```verify:cli
cargo test -p aicortex-types --locked --test coordination
cargo test -p aicortex-store --locked --test coordination
cargo test -p aicortex-store --locked --test coordination_recovery
cargo test -p aicortex-store --locked --test coordination_erasure
cargo test -p aicortex-api --locked --test coordination
cargo test -p aicortex-ingest --locked --test coordination
cargo test -p aicortex-ingest --locked --test coordination_portable
```
