//! Spec 050 B-2, FR-003: the parts a claim is built from cannot be written
//! without a provenance. The field is required, there is no `Default`, and
//! `Claim` itself is `#[non_exhaustive]`, so this literal is the only
//! builder and it does not compile with the field left out.

use aicortex_types::{Claim, ClaimParts};

fn build(other: ClaimParts) -> Claim {
    Claim::new(ClaimParts {
        id: other.id,
        scope: other.scope,
        subject: other.subject,
        predicate: other.predicate,
        value: other.value,
        slot: other.slot,
        epistemic: other.epistemic,
    })
}

fn main() {
    let _ = build;
}
