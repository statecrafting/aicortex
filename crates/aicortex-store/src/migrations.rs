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

/// The lifecycle columns and tables of spec 014, appended by that spec under
/// an `extends` edge on this file.
///
/// Three kinds of thing, and none of them touches what a memory *says*: the
/// columns a lifecycle predicate needs on the memory row (012 D-3's rule for
/// why a column exists at all), the per-capture source log a merge appends to
/// (014 B-2), and the journal a resumable scope erasure writes its progress
/// to (014 B-9).
pub const LIFECYCLE_VERSION: u32 = 4;

/// Durable erasure accounting and resumable fingerprint progress (spec 014).
pub const ERASURE_RECEIPTS_VERSION: u32 = 5;

/// The version an up-to-date store records, which is the highest below.
///
/// `aicortex migrate` reports reaching it (AC-2), and `aicortex serve`
/// refuses with the chassis's stale exit code against a store below it.
pub const EXPECTED_SCHEMA_VERSION: u32 = ERASURE_RECEIPTS_VERSION;

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

/// When a memory stops being true (spec 014 B-5).
///
/// A column rather than a field of the record, because the record of 011 has
/// no `valid_until` and 014 may not amend another spec's type to get one
/// (014 D-3). It is read by a predicate and by nothing else, which is exactly
/// the test 012 D-3 sets for a column existing at all.
const MEMORY_VALID_UNTIL: &str = "ALTER TABLE memory ADD COLUMN valid_until INTEGER";

/// Whether this memory's origin has been erased (spec 014 B-8).
///
/// Erasing a memory does not erase what was derived from it, because a
/// derived summary may be independently valuable; it marks the derivative so
/// that a recall trace can say the citation's origin is gone rather than
/// present a derived claim as if its source were still standing.
const MEMORY_ORIGIN_ERASED: &str =
    "ALTER TABLE memory ADD COLUMN origin_erased INTEGER NOT NULL DEFAULT 0";

/// The fencing token of the lease that last wrote the row.
///
/// Required by [`rahi_store::StoreHandle::fenced_txn`], which is how spec 014
/// B-5's expiry pass runs: a superseded lease holder's statements match no
/// row rather than racing the holder that replaced it. `0` is the value a row
/// written outside any lease carries, and it is below every minted token, so
/// an ordinary capture is never fenced out of its own row.
const MEMORY_FENCE: &str = "ALTER TABLE memory ADD COLUMN fence INTEGER NOT NULL DEFAULT 0";

/// The same column on the counters, for the same reason.
///
/// A leased pass that moved a memory between states without moving the
/// counter in the same transaction would leave `stats` wrong until somebody
/// recounted, and recounting is the scan this schema exists to avoid (B-6).
const COUNTER_FENCE: &str = "ALTER TABLE scope_counter ADD COLUMN fence INTEGER NOT NULL DEFAULT 0";

/// The expiry sweep's index (spec 014 B-5).
///
/// Leads with `status` so the sweep reads only the memories that could
/// expire, and carries `valid_until` so the due ones are a range scan rather
/// than a filter over all of them.
const EXPIRY_INDEX: &str = "CREATE INDEX IF NOT EXISTS memory_status_valid_until
    ON memory (status, valid_until)";

/// One row per *capture*, as opposed to one row per memory (spec 014 B-2).
///
/// 012 B-2 keeps `provenance` at one row per memory, and that stays true:
/// that row is the projection of the provenance the record carries. This
/// table is the other question, which a merge makes askable for the first
/// time: a repeated capture does not insert a second memory, so without this
/// the second capture's source would simply be lost. `ordinal` is `0` for the
/// capture that created the row and rises with each merge, so the order the
/// sources arrived in survives.
const SOURCE_TABLE: &str = "CREATE TABLE IF NOT EXISTS memory_source (
    memory_id TEXT NOT NULL REFERENCES memory (id),
    ordinal INTEGER NOT NULL,
    scope_id TEXT NOT NULL,
    source_system TEXT NOT NULL,
    source_external_id TEXT,
    source_locator TEXT,
    captured_at INTEGER NOT NULL,
    ingested_at INTEGER NOT NULL,
    extractor_name TEXT,
    extractor_version TEXT,
    PRIMARY KEY (memory_id, ordinal)
)";

/// The source log's index: every source of one memory, under its scope.
const SOURCE_INDEX: &str = "CREATE INDEX IF NOT EXISTS memory_source_scope_memory
    ON memory_source (scope_id, memory_id)";

/// What a scope erasure has done so far (spec 014 B-9).
///
/// A scope erasure runs in bounded batches under a lease and must survive the
/// process that started it. Resumption itself needs no state, because the
/// work is defined by what is left rather than by a cursor: the next batch is
/// the next memories in the scope that are not erased yet, so a crash loses
/// at most the batch that was in flight and a rerun is idempotent. What the
/// journal adds is the per-batch progress B-9 requires to be reportable, and
/// the batch number the single completion Decision counts. Version 5 adds
/// operation identity and key counts; all progress then commits atomically
/// with the destructive statements.
const ERASURE_JOURNAL_TABLE: &str = "CREATE TABLE IF NOT EXISTS erasure_journal (
    scope_id TEXT NOT NULL,
    batch INTEGER NOT NULL,
    memories INTEGER NOT NULL,
    derivatives INTEGER NOT NULL,
    at INTEGER NOT NULL,
    PRIMARY KEY (scope_id, batch)
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
        rahi_store::coordination_migration(COORDINATION_VERSION),
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
        ),
        Migration::new(
            DECISION_KEY_VERSION,
            "aicortex decision digest keys",
            [DECISION_KEY_TABLE, DECISION_KEY_INDEX].join(";\n"),
        ),
        // `ALTER TABLE` has no `IF NOT EXISTS` in SQLite, so unlike the
        // migrations above this one is not idempotent on its own. It does not
        // need to be: the chassis reads `schema_version` and applies only the
        // versions above what the store records (`rahi_store::migrate`), so a
        // rerun against a store that already carries version 4 applies
        // nothing at all.
        Migration::new(
            LIFECYCLE_VERSION,
            "aicortex lifecycle columns, source log and erasure journal",
            [
                MEMORY_VALID_UNTIL,
                MEMORY_ORIGIN_ERASED,
                MEMORY_FENCE,
                COUNTER_FENCE,
                EXPIRY_INDEX,
                SOURCE_TABLE,
                SOURCE_INDEX,
                ERASURE_JOURNAL_TABLE,
            ]
            .join(";\n"),
        ),
        Migration::new(
            ERASURE_RECEIPTS_VERSION,
            "aicortex durable erasure receipts and fingerprint progress",
            "CREATE TABLE erasure_receipt (
                scope_id TEXT NOT NULL,
                target TEXT NOT NULL,
                decision_id TEXT NOT NULL UNIQUE,
                decision TEXT NOT NULL,
                ready INTEGER NOT NULL DEFAULT 0,
                delivered INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (scope_id, target)
            );
            ALTER TABLE erasure_journal ADD COLUMN operation TEXT NOT NULL DEFAULT 'legacy';
            ALTER TABLE erasure_journal ADD COLUMN keys_destroyed INTEGER NOT NULL DEFAULT 0;
            ALTER TABLE erasure_journal ADD COLUMN marked INTEGER NOT NULL DEFAULT 0;
            ALTER TABLE memory ADD COLUMN fingerprint_version INTEGER NOT NULL DEFAULT 0;
            CREATE INDEX memory_redigest ON memory (scope_id, fingerprint_version, id);",
        ),
    ]
});
