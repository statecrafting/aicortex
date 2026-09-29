//! The aicortex binary (spec 010 B-3).
//!
//! The chassis owns argument parsing, boot, logging, the listener, every
//! verb, and the exit codes. The cell is the only thing handed to it.

#![forbid(unsafe_code)]

fn main() {
    rahi_cli::run(aicortex::Aicortex)
}
