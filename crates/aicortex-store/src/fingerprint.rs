//! What makes two captures the same memory (spec 014 B-1, B-10).
//!
//! The predecessor deduplicated on exact normalized text and had nothing
//! else (`openbrain://exact-dedup-only`). Exact dedup is not the defect; the
//! defect was that it was the *only* lifecycle, so everything a fingerprint
//! cannot see accumulated forever. This module keeps the exact half and is
//! explicit that it is the exact half: [`fingerprint`] answers "these are the
//! same bytes in the same scope", and nothing more. Semantic near-duplicates
//! need embeddings and belong to the curator of spec 034 (B-10), which is
//! said here rather than half-solved here.
//!
//! Three properties, each of which is a defect if it is missing.
//!
//! **The scope is mixed in.** The same sentence captured into two scopes is
//! two memories, because a scope is somebody's; a digest over the body alone
//! would make one subject's capture collide with another's. The unique index
//! of 012 B-2 is `(scope_id, fingerprint)` and already separates them, so
//! mixing the scope in is belt as well as braces: it means the value itself
//! is wrong to compare across scopes, and a later index or cache that forgets
//! the column cannot silently reintroduce the collision.
//!
//! **The text is gate-normalized first.** Two captures that differ only in
//! trailing whitespace, Unicode form, or a zero-width character are the same
//! claim, and the gate of spec 013 already owns what that means
//! ([`aicortex_gate::normalize`]). Digesting the raw text would make the
//! merge of B-2 depend on how a client happened to encode a space.
//!
//! **The kind is mixed in.** A fact and a task that read alike are not the
//! same memory, and merging them would lose one of them.
//!
//! # Not the ledger's digest
//!
//! This is a plain BLAKE3 over content, so it is *not* safe to put in the
//! decision chain: an unkeyed digest of a short body can be confirmed by
//! guessing, which is why spec 013 B-9's Decision digest is keyed
//! ([`crate::DecisionKey`]) and why erasure destroys the key (B-11). This
//! value never leaves the `fingerprint` column, and erasure deletes the row's
//! fingerprint with the rest of it (B-7).

use aicortex_types::{Memory, MemoryKind, Scope};

/// The separator between the mixed-in parts.
///
/// `0x1f`, the ASCII unit separator, for the same reason [`crate::ScopeId`]
/// and the cursor's filter digest use it: it cannot occur in a scope
/// rendering or a kind label, so the concatenation is unambiguous and two
/// different tuples cannot render to the same bytes.
const SEPARATOR: u8 = 0x1f;

/// The 32-byte content fingerprint of `text` in `scope` (B-1).
///
/// `text` must already be gate-normalized; [`of_memory`] is the call that
/// guarantees it. Taking the normalized text rather than a [`Memory`] is what
/// lets a caller ask "would this capture merge?" before it has built a record.
#[must_use]
pub fn fingerprint(scope: &Scope, kind: MemoryKind, normalized_text: &str) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(scope.to_string().as_bytes());
    hasher.update(&[SEPARATOR]);
    hasher.update(kind.label().as_bytes());
    hasher.update(&[SEPARATOR]);
    hasher.update(normalized_text.as_bytes());
    *hasher.finalize().as_bytes()
}

/// The same value as the `fingerprint` column holds it: lowercase hex.
///
/// The column is `TEXT` (012 B-2) because every other derived identity in
/// this schema is hex text and a single blob column among them is a shape
/// nobody expects. The rendering is total and injective, so the uniqueness
/// the index enforces is exactly the uniqueness [`fingerprint`] computes.
#[must_use]
pub fn hex(digest: &[u8; 32]) -> String {
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use core::fmt::Write as _;
        // Writing to a `String` is infallible; the result is discarded rather
        // than unwrapped, which the crate's lints forbid.
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// The fingerprint of a memory, as the column holds it.
///
/// Normalizes the body text through the gate before digesting, so the value
/// does not depend on whether this particular memory arrived through
/// [`aicortex_gate::Gate::evaluate`] or was read back from a row. The gate's
/// normalization is idempotent, so applying it to an already-admitted body is
/// the identity and costs one pass.
#[must_use]
pub fn of_memory(memory: &Memory) -> String {
    let normalized = aicortex_gate::normalize::text(&memory.body.text);
    hex(&fingerprint(&memory.scope, memory.kind, &normalized))
}

/// The fingerprint a capture of `text` into `scope` would carry.
///
/// What a merge check asks (B-2) before a record exists.
#[must_use]
pub fn of_text(scope: &Scope, kind: MemoryKind, text: &str) -> String {
    hex(&fingerprint(
        scope,
        kind,
        &aicortex_gate::normalize::text(text),
    ))
}
