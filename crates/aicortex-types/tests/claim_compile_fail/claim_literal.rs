//! Spec 050 B-2, FR-003: `Claim` is `#[non_exhaustive]`, so another crate
//! cannot write it as a struct literal and skip `Claim::new`.

use aicortex_types::{Claim, ClaimParts, CLAIM_SCHEMA_VERSION};

fn build(other: ClaimParts) -> Claim {
    Claim {
        id: other.id,
        scope: other.scope,
        subject: other.subject,
        predicate: other.predicate,
        value: other.value,
        slot: other.slot,
        epistemic: other.epistemic,
        provenance: other.provenance,
        schema_version: CLAIM_SCHEMA_VERSION,
    }
}

fn main() {
    let _ = build;
}
