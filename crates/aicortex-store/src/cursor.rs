//! Keyset paging, and the signature that binds a cursor to one scope and one
//! filter (spec 012 B-7).
//!
//! There is no offset paging in this system. `OFFSET` counts rows rather than
//! remembering them, so a page boundary shifts under a concurrent insert and
//! a listing silently shows a row twice or skips one. A keyset cursor names
//! the last row seen, and the next page is everything strictly after it in
//! the total order `(created desc, id desc)`.
//!
//! The signature is what makes the cursor opaque rather than merely encoded.
//! A caller cannot move a cursor from scope A's listing to scope B's, or from
//! one filter to another, because the scope and the filter are part of what
//! was signed: presenting the cursor under a different pair recomputes a
//! different MAC and the cursor is refused. A tampered position fails the
//! same check. What the cursor is not is a capability: it carries no
//! authority of its own, and every read that accepts one still names its
//! scope in the predicate (B-3).

use aicortex_types::MemoryId;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rahi_types::{Error, UnixSeconds};
use ring::hmac;

use crate::memory_repo::MemoryFilter;
use crate::scope_repo::ScopeId;

/// The bytes of a cursor's position: a version, a timestamp, and an id.
const POSITION_BYTES: usize = 1 + 8 + 16;
/// SHA-256's output, which is the MAC's width.
const MAC_BYTES: usize = 32;
/// The wire format's version byte, so a later shape is distinguishable
/// rather than merely malformed.
const VERSION: u8 = 1;
/// What the MAC is computed over besides the position, so a cursor signed by
/// this product for this purpose cannot be replayed into another one that
/// happens to share the key.
const DOMAIN: &[u8] = b"aicortex:012:keyset-cursor:v1";

/// The secret a cursor is signed with.
///
/// Deployment-wide rather than per-process: a cursor handed out by one
/// replica is presented to another, and a key minted at boot would refuse
/// every cursor after a restart or a failover. Where the bytes come from is
/// the surface's business (spec 020); what this crate owns is that they are
/// 32 bytes of secret and that nothing derives a cursor without them.
pub struct CursorKey(hmac::Key);

impl CursorKey {
    /// The key material's width.
    pub const BYTES: usize = 32;

    /// Take a key from 32 secret bytes.
    #[must_use]
    pub fn new(secret: [u8; Self::BYTES]) -> Self {
        Self(hmac::Key::new(hmac::HMAC_SHA256, &secret))
    }

    /// Take a key from secret material of any length, by digesting it first.
    ///
    /// For a caller whose secret is a custodied string rather than a 32-byte
    /// array. The digest is not a stretch and does not make a weak secret
    /// strong; it makes a well-formed one the right width.
    #[must_use]
    pub fn from_secret(secret: &[u8]) -> Self {
        let digest = ring::digest::digest(&ring::digest::SHA256, secret);
        Self(hmac::Key::new(hmac::HMAC_SHA256, digest.as_ref()))
    }
}

impl core::fmt::Debug for CursorKey {
    /// Never the key material: a cursor key in a log is a cursor key in a
    /// log.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("CursorKey(..)")
    }
}

/// The last row a page ended on: the pair the next page starts after.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cursor {
    /// The `created` of the last row returned.
    pub created: UnixSeconds,
    /// The id of the last row returned, which breaks a tie on `created`.
    pub id: MemoryId,
}

impl Cursor {
    /// The position after `created` and `id`.
    #[must_use]
    pub const fn new(created: UnixSeconds, id: MemoryId) -> Self {
        Self { created, id }
    }

    /// Render the cursor as the opaque base64url string a client carries.
    #[must_use]
    pub fn encode(&self, key: &CursorKey, scope: &ScopeId, filter: &MemoryFilter) -> String {
        let position = self.position();
        let mac = sign(key, &position, scope, filter);
        let mut wire = Vec::with_capacity(POSITION_BYTES + MAC_BYTES);
        wire.extend_from_slice(&position);
        wire.extend_from_slice(mac.as_ref());
        URL_SAFE_NO_PAD.encode(wire)
    }

    /// Read a cursor a client presented, under the scope and filter it is
    /// being presented with.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] when the text is not base64url, is the wrong
    /// length, carries an unknown version, or does not verify under this key
    /// for this scope and this filter. Every one of those is the same answer
    /// to the caller: the cursor is not usable here. They are distinguished
    /// in the message, not in the type, because a client that is told which
    /// check failed learns something about the key.
    pub fn decode(
        text: &str,
        key: &CursorKey,
        scope: &ScopeId,
        filter: &MemoryFilter,
    ) -> Result<Self, Error> {
        let refused =
            |reason: &str| Error::Validation(format!("cursor is not usable here: {reason}"));
        let wire = URL_SAFE_NO_PAD
            .decode(text)
            .map_err(|_| refused("it is not base64url"))?;
        if wire.len() != POSITION_BYTES + MAC_BYTES {
            return Err(refused("it is the wrong length"));
        }
        let (position, mac) = wire.split_at(POSITION_BYTES);
        hmac::verify(signing_key(key), &material(position, scope, filter), mac)
            .map_err(|_| refused("it was not signed for this scope and filter"))?;

        let mut version = [0u8; 1];
        let mut created = [0u8; 8];
        let mut id = [0u8; 16];
        // The split points are the widths above and the slice's length was
        // just checked, so neither `copy_from_slice` can panic on a width
        // mismatch.
        let (version_bytes, rest) = position.split_at(1);
        let (created_bytes, id_bytes) = rest.split_at(8);
        version.copy_from_slice(version_bytes);
        created.copy_from_slice(created_bytes);
        id.copy_from_slice(id_bytes);
        if version[0] != VERSION {
            return Err(refused("it carries a version this build does not read"));
        }
        Ok(Self {
            created: UnixSeconds::new(u64::from_be_bytes(created)),
            id: MemoryId::from_uuid(uuid::Uuid::from_bytes(id)),
        })
    }

    /// The signed position: version, timestamp, id.
    fn position(&self) -> [u8; POSITION_BYTES] {
        let mut bytes = [0u8; POSITION_BYTES];
        let (version, rest) = bytes.split_at_mut(1);
        let (created, id) = rest.split_at_mut(8);
        version.copy_from_slice(&[VERSION]);
        created.copy_from_slice(&self.created.get().to_be_bytes());
        id.copy_from_slice(self.id.as_uuid().as_bytes());
        bytes
    }
}

/// The key, as `ring` holds it.
fn signing_key(key: &CursorKey) -> &hmac::Key {
    &key.0
}

/// Everything the MAC covers: the domain, the position, the scope, and the
/// filter. The scope and the filter are covered but not carried, so a cursor
/// presented under a different pair fails verification rather than being
/// silently re-interpreted.
fn material(position: &[u8], scope: &ScopeId, filter: &MemoryFilter) -> Vec<u8> {
    let mut material = Vec::with_capacity(DOMAIN.len() + position.len() + 128);
    material.extend_from_slice(DOMAIN);
    material.extend_from_slice(position);
    material.extend_from_slice(scope.as_str().as_bytes());
    material.extend_from_slice(filter.digest().as_bytes());
    material
}

/// Sign a position for one scope and one filter.
fn sign(key: &CursorKey, position: &[u8], scope: &ScopeId, filter: &MemoryFilter) -> hmac::Tag {
    hmac::sign(signing_key(key), &material(position, scope, filter))
}
