//! The digest keys a refusal's Decision is written under (spec 013 B-9, D-2).
//!
//! B-9 requires every refusal and every quarantine to append a Decision
//! carrying a digest of the normalized content, and D-2 requires that digest
//! to be *keyed*. The reason is constitution XIII. The decision chain is
//! append-only, so anything in it is there forever; an unkeyed hash of a
//! short or predictable body can be confirmed by guessing, which would put
//! memory content into the chain in effect if not in letter.
//!
//! The construction is HMAC-SHA-256 under a key minted for that one Decision.
//! The Decision carries the digest, the algorithm identifier and the key id.
//! The key itself lives here, in one application-store row naming the scope
//! and, for a quarantine, the memory. There is no shared key and no permanent
//! key, so destroying one row makes exactly one digest unverifiable and
//! leaves the chain intact.
//!
//! Spec 014's erasure destroys a key together with the object it covers, and
//! the two staging calls it will use are here
//! ([`DecisionKeyRepo::stage_destroy_for_memory`],
//! [`DecisionKeyRepo::stage_destroy_for_scope`]). Once a key row is gone the
//! chained digest is an opaque value, and nothing in this crate recomputes or
//! returns the bytes it was taken over: there is no method here that reads
//! content, and the key was the only thing that could confirm a guess
//! (013 FR-007).
//!
//! That is a claim about the *row*, and it is deliberately not the larger
//! claim. Spec 013 FR-008: no surface and no document here says that erasure
//! reaches refusal and quarantine records, and none may until spec 014's own
//! tests show the key destroyed, a backup taken after the erasure holding no
//! key, and no replay (capture, outbox or import) restoring it. A backup
//! taken before an erasure still holds the key until that backup is
//! discarded.
//!
//! This file is spec 013's, in spec 012's crate, because B-9 puts the key row
//! in the application store and the gate stays pure (013 section 2, D-5).

use aicortex_types::{MemoryId, Scope};
use rahi_store::{Statement, StoreHandle, TxnBuilder, Value};
use rahi_types::{Error, UnixSeconds};
use ring::hmac;
use ring::rand::{SecureRandom, SystemRandom};
use serde::Deserialize;

use crate::scope_repo::{ScopeId, seconds_to_sql};

/// The algorithm identifier every digest is stamped with (B-9).
///
/// Written beside the digest rather than assumed, because D-2 allows a later
/// decision to change the construction only by recording a new identifier
/// beside new digests, never by reinterpreting old ones.
pub const DIGEST_ALGORITHM: &str = "HMAC-SHA-256";

/// The length of a minted key, in bytes: a full SHA-256 block's worth of
/// entropy, so the key is never the weak part of the digest.
const KEY_BYTES: usize = 32;

/// The length of a key id, in bytes before hex encoding.
const ID_BYTES: usize = 16;

/// The id of one Decision's digest key.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DecisionKeyId(String);

impl DecisionKeyId {
    /// The id a Decision carries, read back.
    ///
    /// No validation: the id is this crate's own minting and the row is keyed
    /// on it, so a value that was never minted simply finds nothing.
    #[must_use]
    pub fn parse(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// The id, as the Decision carries it and the row is keyed on.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl core::fmt::Display for DecisionKeyId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.0)
    }
}

/// One Decision's digest key: the id that is public and the secret that is
/// not.
///
/// [`Debug`] is written by hand and prints the id only. A derived `Debug`
/// would put the key bytes into the first log line somebody adds.
#[derive(Clone, PartialEq, Eq)]
pub struct DecisionKey {
    id: DecisionKeyId,
    secret: Vec<u8>,
}

impl core::fmt::Debug for DecisionKey {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("DecisionKey")
            .field("id", &self.id)
            .field("secret", &"<redacted>")
            .finish()
    }
}

impl DecisionKey {
    /// Mint a fresh key for one Decision (B-9).
    ///
    /// A new key every time, which is what FR-007 asserts: two refusals of
    /// the same candidate mint two keys and so produce two different digests,
    /// and a replayed candidate never recovers an earlier Decision's key.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] when the system random source fails, which is not a
    /// condition to carry on through: a predictable digest key is worse than
    /// no digest.
    pub fn mint() -> Result<Self, Error> {
        let random = SystemRandom::new();
        let mut secret = vec![0u8; KEY_BYTES];
        let mut id = [0u8; ID_BYTES];
        random
            .fill(&mut secret)
            .and_then(|()| random.fill(&mut id))
            .map_err(|_| {
                Error::Io("the system random source would not mint a digest key".to_owned())
            })?;
        Ok(Self {
            id: DecisionKeyId(hex(&id)),
            secret,
        })
    }

    /// The id, which the Decision carries.
    #[must_use]
    pub const fn id(&self) -> &DecisionKeyId {
        &self.id
    }

    /// The keyed digest of `material`, lowercase hex (B-9).
    #[must_use]
    pub fn digest(&self, material: &[u8]) -> String {
        let key = hmac::Key::new(hmac::HMAC_SHA256, &self.secret);
        hex(hmac::sign(&key, material).as_ref())
    }
}

/// The `decision_key` table.
///
/// Every statement names `scope_id`, like every other statement in this crate
/// (spec 012 B-3): a key is an object of somebody's scope, and a read that
/// could reach across scopes would be a way to verify another subject's
/// refusal digests.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DecisionKeyRepo;

const INSERT_SQL: &str = "INSERT INTO decision_key (
        key_id, scope_id, memory_id, algorithm, secret, created
    ) VALUES ($1, $2, $3, $4, $5, $6)";

const GET_SQL: &str = "SELECT secret FROM decision_key WHERE scope_id = $1 AND key_id = $2";

const DESTROY_MEMORY_SQL: &str = "DELETE FROM decision_key WHERE scope_id = $1 AND memory_id = $2";

const DESTROY_SCOPE_SQL: &str = "DELETE FROM decision_key WHERE scope_id = $1";

/// One key row, read back.
///
/// [`Debug`] is written by hand for the same reason [`DecisionKey`]'s is, one
/// struct above: a derived one would put the key bytes into the first log
/// line somebody adds, and a private struct is no protection against that
/// because the log line is written by whoever holds the value.
#[derive(Deserialize)]
struct SecretRow {
    secret: Vec<u8>,
}

impl core::fmt::Debug for SecretRow {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("SecretRow")
            .field("secret", &"<redacted>")
            .finish()
    }
}

impl DecisionKeyRepo {
    /// Stage the key row for one Decision, in the caller's transaction.
    ///
    /// `memory` is `Some` exactly when the verdict stored a row, which is
    /// what lets the erasure of a quarantined memory find the key that covers
    /// it. A refusal stores nothing, so its key is reachable only through the
    /// scope, and erasing the scope destroys it.
    pub fn stage(
        txn: &mut TxnBuilder,
        scope: &Scope,
        key: &DecisionKey,
        memory: Option<MemoryId>,
        at: UnixSeconds,
    ) {
        let scope_id = ScopeId::of(scope);
        txn.push(Statement::with_params(
            INSERT_SQL,
            vec![
                Value::from(key.id().as_str()),
                Value::from(&scope_id),
                Value::from(memory.map(|id| id.to_string())),
                Value::from(DIGEST_ALGORITHM),
                Value::Blob(key.secret.clone()),
                Value::Integer(seconds_to_sql(at)),
            ],
        ));
    }

    /// The key a Decision names, when the row still exists.
    ///
    /// `query_consistent`, the leader: the answer decides whether a digest
    /// can be confirmed, and a stale replica that still held a destroyed key
    /// would answer a question erasure has already closed.
    ///
    /// # Errors
    ///
    /// The store's error.
    pub async fn get(
        store: &StoreHandle,
        scope: &Scope,
        id: &DecisionKeyId,
    ) -> Result<Option<DecisionKey>, Error> {
        let scope_id = ScopeId::of(scope);
        let rows: Vec<SecretRow> = store
            .query_consistent(
                GET_SQL,
                vec![Value::from(&scope_id), Value::from(id.as_str())],
            )
            .await?;
        Ok(rows.into_iter().next().map(|row| DecisionKey {
            id: id.clone(),
            secret: row.secret,
        }))
    }

    /// Stage the destruction of every key covering `memory` (spec 014 B-7).
    pub fn stage_destroy_for_memory(txn: &mut TxnBuilder, scope: &Scope, memory: MemoryId) {
        let scope_id = ScopeId::of(scope);
        txn.push(Statement::with_params(
            DESTROY_MEMORY_SQL,
            vec![Value::from(&scope_id), Value::from(memory.to_string())],
        ));
    }

    /// Stage the destruction of every key in `scope` (spec 014 B-9).
    pub fn stage_destroy_for_scope(txn: &mut TxnBuilder, scope: &Scope) {
        let scope_id = ScopeId::of(scope);
        txn.push(Statement::with_params(
            DESTROY_SCOPE_SQL,
            vec![Value::from(&scope_id)],
        ));
    }
}

/// Lowercase hex, written here rather than through [`crate::hex_digest`]
/// because that function digests and this one only renders.
fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use core::fmt::Write as _;
        // Writing to a `String` is infallible; the result is discarded rather
        // than unwrapped, which the crate's lints forbid.
        let _ = write!(out, "{byte:02x}");
    }
    out
}
