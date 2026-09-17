//! B-5, FR-003: the struct literal is not a second way in either. Both fields
//! of `Promotion` are private, so `Promotion::new` is the only constructor.

use aicortex_types::{DecisionRef, Promotion};
use rahi_types::Sub;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _ = Promotion {
        by: Sub::new("not-a-human"),
        decision: DecisionRef::new("01JC5X6Q0000000000000000")?,
    };
    Ok(())
}
