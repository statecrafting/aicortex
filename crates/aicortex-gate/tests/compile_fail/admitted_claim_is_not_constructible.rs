//! Spec 051 FR-002: an `AdmittedClaim` cannot be made outside the gate.
//!
//! Its fields are private and its constructor is crate-private, so the only
//! way to one is `ClaimVerdict::Admit`. A claim is a value anybody can build;
//! the admission is not.

use aicortex_gate::AdmittedClaim;
use aicortex_types::Claim;

fn forge(claim: Claim) -> AdmittedClaim {
    AdmittedClaim { claim }
}

fn main() {}
