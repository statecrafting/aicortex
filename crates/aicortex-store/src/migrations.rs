//! The schema, as an ordered list of rahi migrations (spec 012 B-1, B-2).
//!
//! The predecessor had no migrations at all: its schema was SQL pasted from a
//! document, so every deployment was a different shape and every later
//! extension had to guess the prior state (`openbrain://no-schema-management`).
//! Here the schema is a list of versioned values the chassis applies through
//! its `migrate` verb, and `serve` refuses to start behind it
//! (`rahi://030`). A migration that has shipped is never edited: a mistake is
//! a new migration with the next version.
//!
//! Every later spec that owns a table appends to [`migrations`] with an
//! `extends` edge on this file and a version above the last one here.

use rahi_store::Migration;

/// The chassis's own coordination tables: the lease fence and the outbox.
///
/// The outbox is not an optional extra here. Spec 012 B-4 commits a memory
/// and the work its capture implies in one transaction, and the only way a
/// notify becomes durable with the row it describes is
/// [`rahi_store::Outbox::stage`], which needs this table
/// (`rahi_store::coordination_migration`). An app that stages an envelope
/// carries this migration, so this one does.
pub const COORDINATION_VERSION: u32 = 1;

/// The memory schema of B-2: the five tables and their three indexes.
///
/// Distinct from `aicortex_types::MEMORY_SCHEMA_VERSION`, which versions the
/// shape of one record; this versions the shape of the database.
pub const MEMORY_TABLES_VERSION: u32 = 2;

/// The digest-key table of spec 013 B-9, appended by that spec under an
/// `extends` edge on this file.
///
/// Its own migration rather than a column on an existing table: the keys are
/// a different object with a different lifetime from every memory row, and
/// erasure destroys them on their own terms (013 FR-007, 014 B-7, B-9).
pub const DECISION_KEY_VERSION: u32 = 3;

/// The predicate registry of spec 050 B-15, appended by that spec under an
/// `extends` edge on this file.
pub const PREDICATE_REGISTRY_VERSION: u32 = 4;

/// Claim proposals, admission records and admission policies of spec 051,
/// appended by that spec under an `extends` edge on this file.
pub const CLAIM_ADMISSION_VERSION: u32 = 5;

/// The version an up-to-date store records, which is the highest below.
///
/// `aicortex migrate` reports reaching it (AC-2), and `aicortex serve`
/// refuses with the chassis's stale exit code against a store below it.
pub const EXPECTED_SCHEMA_VERSION: u32 = CLAIM_ADMISSION_VERSION;

/// The scope a memory lives in (B-2).
///
/// `scope_id` is the digest [`crate::ScopeId`] derives from the owner, the
/// kind and the key, so a writer computes it without a read and two replicas
/// agree on it without coordinating. The natural key is unique as well, so a
/// second row for the same scope is a constraint violation rather than a
/// duplicate: `key` is the empty string, never `NULL`, because SQLite treats
/// two `NULL`s in a unique index as distinct and would admit both.
const SCOPE_TABLE: &str = "CREATE TABLE IF NOT EXISTS scope (
    scope_id TEXT PRIMARY KEY,
    owner TEXT NOT NULL,
    kind TEXT NOT NULL,
    key TEXT NOT NULL,
    created INTEGER NOT NULL,
    UNIQUE (owner, kind, key)
)";

/// The record of 011, one row, body inline (B-2, D-1, D-3).
///
/// `record` is the memory's own serde JSON and is the single source of truth
/// for what the memory says. Every other column is a projection of it,
/// written by the same function in the same statement, so the two cannot
/// disagree: they exist because a predicate, an index, or a counter needs the
/// value in a column rather than inside a document (D-3).
const MEMORY_TABLE: &str = "CREATE TABLE IF NOT EXISTS memory (
    id TEXT PRIMARY KEY,
    scope_id TEXT NOT NULL REFERENCES scope (scope_id),
    status TEXT NOT NULL,
    superseded_by TEXT,
    kind TEXT NOT NULL,
    trust TEXT NOT NULL,
    fingerprint TEXT NOT NULL,
    schema_version INTEGER NOT NULL,
    body_bytes INTEGER NOT NULL,
    created INTEGER NOT NULL,
    updated INTEGER NOT NULL,
    record TEXT NOT NULL
)";

/// Where a claim came from (B-2).
///
/// One row per memory, carrying its own `scope_id` so that a provenance read
/// states the scope in its own predicate rather than reaching the memory
/// table for it (B-3, B-9).
const PROVENANCE_TABLE: &str = "CREATE TABLE IF NOT EXISTS provenance (
    memory_id TEXT PRIMARY KEY REFERENCES memory (id),
    scope_id TEXT NOT NULL,
    source_system TEXT NOT NULL,
    source_external_id TEXT,
    source_locator TEXT,
    captured_at INTEGER NOT NULL,
    ingested_at INTEGER NOT NULL,
    extractor_name TEXT,
    extractor_version TEXT
)";

/// `derived_from` as a child table (B-2).
///
/// `position` preserves the order the parents were named in, so a record
/// round-trips through the store unchanged. `scope_id` is the child's, for
/// the same reason the provenance row carries one: a derivation is read under
/// a scope predicate, and a parent in another scope is not reachable through
/// it (B-9).
const DERIVATION_TABLE: &str = "CREATE TABLE IF NOT EXISTS memory_derivation (
    memory_id TEXT NOT NULL REFERENCES memory (id),
    parent_id TEXT NOT NULL,
    position INTEGER NOT NULL,
    scope_id TEXT NOT NULL,
    PRIMARY KEY (memory_id, parent_id)
)";

/// The aggregates, maintained rather than computed (B-6).
///
/// The predecessor's statistics endpoint read every row's metadata into
/// memory to count things (`openbrain://stats-loads-everything`). This table
/// is the answer: one row per scope, kind and status, updated in the same
/// transaction as the row that changes it, so `stats` is a bounded read
/// whose cost does not grow with the number of memories.
const COUNTER_TABLE: &str = "CREATE TABLE IF NOT EXISTS scope_counter (
    scope_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    status TEXT NOT NULL,
    count INTEGER NOT NULL,
    PRIMARY KEY (scope_id, kind, status)
)";

/// The listing index: the keyset order of B-7, inside one scope.
const LISTING_INDEX: &str = "CREATE INDEX IF NOT EXISTS memory_scope_status_created
    ON memory (scope_id, status, created DESC)";

/// Uniqueness among the memories that still exist (B-2).
///
/// Partial, because an erased memory's fingerprint must not keep a new
/// capture of the same content out: erasure is real (constitution XIV), and a
/// tombstone that reserved the content's identity would be a record of what
/// was forgotten.
const FINGERPRINT_INDEX: &str = "CREATE UNIQUE INDEX IF NOT EXISTS memory_scope_fingerprint
    ON memory (scope_id, fingerprint) WHERE status <> 'erased'";

/// The curators' index (B-2): what changed, by state, across scopes.
///
/// The one index that does not lead with `scope_id`, because the workers of
/// spec 034 sweep by state. It is an index, not a licence: a read still names
/// its scope in the predicate (B-3).
const CURATOR_INDEX: &str = "CREATE INDEX IF NOT EXISTS memory_status_updated
    ON memory (status, updated)";

/// One Decision's digest key (spec 013 B-9).
///
/// `secret` is the HMAC key and `algorithm` is the identifier stamped beside
/// the digest, so a later construction can be told from this one rather than
/// having to be guessed. `memory_id` is present exactly when the Decision
/// covers a stored row, which is how the erasure of a quarantined memory
/// finds the key to destroy; a refusal stores no row and its key is reachable
/// through the scope alone.
const DECISION_KEY_TABLE: &str = "CREATE TABLE IF NOT EXISTS decision_key (
    key_id TEXT PRIMARY KEY,
    scope_id TEXT NOT NULL,
    memory_id TEXT,
    algorithm TEXT NOT NULL,
    secret BLOB NOT NULL,
    created INTEGER NOT NULL
)";

/// Erasure's index: every key of a scope, and every key of a memory.
///
/// Leading with `scope_id` for the same reason every other index here does: a
/// destruction names its scope in the predicate, and the index that serves it
/// must not invite a statement that does not (spec 012 B-3).
const DECISION_KEY_INDEX: &str = "CREATE INDEX IF NOT EXISTS decision_key_scope_memory
    ON decision_key (scope_id, memory_id)";

/// One registered version of one namespace's vocabulary (spec 050 B-15).
///
/// Not scoped: a vocabulary is shared by every scope that writes claims in
/// its namespace, and it is not memory content. `document` is the set's own
/// serde JSON and `digest` is what the registration's Decision names. The
/// primary key makes a version immutable at the table as well as in the
/// rule: a second insert of one version fails the transaction.
const PREDICATE_REGISTRY_TABLE: &str = "CREATE TABLE IF NOT EXISTS predicate_registry (
    namespace TEXT NOT NULL,
    version INTEGER NOT NULL,
    digest TEXT NOT NULL,
    document TEXT NOT NULL,
    registered_by TEXT NOT NULL,
    registered_at INTEGER NOT NULL,
    PRIMARY KEY (namespace, version)
)";

/// One claim proposal (spec 051 B-2): append-only, never read by a
/// projection. `document` is the proposal's serde JSON, evidence included.
const CLAIM_PROPOSAL_TABLE: &str = "CREATE TABLE IF NOT EXISTS claim_proposal (
    proposal_id TEXT PRIMARY KEY,
    scope_id TEXT NOT NULL,
    claim_id TEXT NOT NULL,
    predicate TEXT NOT NULL,
    proposer TEXT NOT NULL,
    proposed_at INTEGER NOT NULL,
    document TEXT NOT NULL
)";

/// The proposals of one claim in one scope.
const CLAIM_PROPOSAL_INDEX: &str = "CREATE INDEX IF NOT EXISTS claim_proposal_scope_claim
    ON claim_proposal (scope_id, claim_id)";

/// One admission (spec 051 B-11): written in the transaction that appends
/// the claim, naming the policy version it was judged under and the
/// evidence it was judged on.
const CLAIM_ADMISSION_TABLE: &str = "CREATE TABLE IF NOT EXISTS claim_admission (
    claim_id TEXT PRIMARY KEY,
    scope_id TEXT NOT NULL,
    proposal_id TEXT NOT NULL,
    policy_id TEXT NOT NULL,
    policy_version INTEGER NOT NULL,
    authority TEXT NOT NULL,
    sourcing TEXT NOT NULL,
    verdict TEXT NOT NULL,
    evidence TEXT NOT NULL,
    relations TEXT NOT NULL,
    corrects TEXT NOT NULL,
    conflicts TEXT NOT NULL,
    admitted_at INTEGER NOT NULL
)";

/// The admission of one proposal in one scope.
const CLAIM_ADMISSION_INDEX: &str = "CREATE INDEX IF NOT EXISTS claim_admission_scope_proposal
    ON claim_admission (scope_id, proposal_id)";

/// One version of one admission policy (spec 051 B-12). Not scoped: a policy
/// is deployment configuration, not memory content. The primary key makes a
/// version immutable.
const ADMISSION_POLICY_TABLE: &str = "CREATE TABLE IF NOT EXISTS admission_policy (
    policy_id TEXT NOT NULL,
    version INTEGER NOT NULL,
    digest TEXT NOT NULL,
    document TEXT NOT NULL,
    registered_by TEXT NOT NULL,
    registered_at INTEGER NOT NULL,
    PRIMARY KEY (policy_id, version)
)";

/// The migrations, in version order (B-1).
///
/// Each is idempotent (`IF NOT EXISTS` throughout), so a rerun against a
/// store that already carries the schema applies nothing, and the chassis
/// records the version it applied.
#[must_use]
pub fn migrations() -> &'static [Migration] {
    LIST.as_slice()
}

static LIST: std::sync::LazyLock<[Migration; 5]> = std::sync::LazyLock::new(|| {
    [
        // Every shipped migration only creates tables and indexes, so each is
        // declared additive (spec 046 B-2, D-2). The declaration is not part
        // of the SQL, so the checksum rahi records is unchanged (046 B-3).
        rahi_store::coordination_migration(COORDINATION_VERSION).additive(),
        Migration::new(
            MEMORY_TABLES_VERSION,
            "aicortex memory schema",
            [
                SCOPE_TABLE,
                MEMORY_TABLE,
                PROVENANCE_TABLE,
                DERIVATION_TABLE,
                COUNTER_TABLE,
                LISTING_INDEX,
                FINGERPRINT_INDEX,
                CURATOR_INDEX,
            ]
            .join(";\n"),
        )
        .additive(),
        Migration::new(
            DECISION_KEY_VERSION,
            "aicortex decision digest keys",
            [DECISION_KEY_TABLE, DECISION_KEY_INDEX].join(";\n"),
        )
        .additive(),
        Migration::new(
            PREDICATE_REGISTRY_VERSION,
            "aicortex predicate registry",
            PREDICATE_REGISTRY_TABLE,
        )
        .additive(),
        Migration::new(
            CLAIM_ADMISSION_VERSION,
            "aicortex claim admission",
            [
                CLAIM_PROPOSAL_TABLE,
                CLAIM_PROPOSAL_INDEX,
                CLAIM_ADMISSION_TABLE,
                CLAIM_ADMISSION_INDEX,
                ADMISSION_POLICY_TABLE,
            ]
            .join(";\n"),
        )
        .additive(),
    ]
});
