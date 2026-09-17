//! B-5, FR-003: the instruction variant cannot be named outside the crate
//! that defines it, so there is no expression that reaches instruction grade
//! without going through `TrustClass::instruction` and its `Promotion`.

use aicortex_types::TrustClass;

fn main() {
    let _ = TrustClass::Instruction;
}
