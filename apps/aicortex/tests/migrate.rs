//! Spec 012 AC-2, against the binary rather than against a library.
//!
//! The crate tests of `aicortex-store` open rahi's single-voter node and
//! exercise the migration list directly (FR-001). AC-2 is a different claim:
//! that the *deployed* verbs behave, so it is asserted here, where
//! `CARGO_BIN_EXE_aicortex` exists, by running the same commands an operator
//! runs.
//!
//! - `aicortex migrate` on an empty data directory reports the expected
//!   schema version, and a rerun applies nothing (B-1).
//! - `aicortex serve` against a store behind the cell's migrations refuses
//!   with the chassis's stale exit code, `2`, rather than migrating at boot
//!   (B-1, `rahi://030`).
//!
//! One data directory, one sequence: the stale `serve` has to run before the
//! `migrate` that closes the gap, because that is the only moment a real
//! deployment is behind.

use std::net::TcpListener;
use std::path::Path;
use std::process::{Command, Output};

type Outcome = Result<(), String>;

/// A port the OS is not using, released before the child takes it.
fn free_port() -> Result<u16, String> {
    let listener = TcpListener::bind("127.0.0.1:0").map_err(|err| err.to_string())?;
    listener
        .local_addr()
        .map(|addr| addr.port())
        .map_err(|err| err.to_string())
}

/// Run a verb against `data_dir` with every `RAHI_*` the cell needs.
fn aicortex(verb: &str, data_dir: &Path, ports: (u16, u16, u16)) -> Result<Output, String> {
    let (app, hiqlite_api, hiqlite_raft) = ports;
    let mut command = Command::new(env!("CARGO_BIN_EXE_aicortex"));
    command.arg(verb).env_clear();
    for (key, value) in [
        ("RAHI_PUBLIC_URL", format!("http://127.0.0.1:{app}")),
        ("RAHI_LISTEN_ADDR", format!("127.0.0.1:{app}")),
        ("RAHI_DATA_DIR", data_dir.display().to_string()),
        ("RAHI_HIQLITE_API_ADDR", format!("127.0.0.1:{hiqlite_api}")),
        (
            "RAHI_HIQLITE_RAFT_ADDR",
            format!("127.0.0.1:{hiqlite_raft}"),
        ),
        ("RAHI_RAUTHY_MODE", "none".to_owned()),
    ] {
        command.env(key, value);
    }
    command
        .output()
        .map_err(|err| format!("aicortex {verb} did not run: {err}"))
}

#[test]
fn ac2_migrate_reaches_the_expected_version_and_serve_refuses_a_stale_store() -> Outcome {
    let data_dir = tempfile::tempdir().map_err(|err| err.to_string())?;
    let ports = (free_port()?, free_port()?, free_port()?);

    // `migrate` and `serve` both need the key set, which is `first-boot`'s
    // job and not this spec's. Its output carries the bootstrap credentials,
    // so only its exit code is read.
    let first_boot = aicortex("first-boot", data_dir.path(), ports)?;
    assert_eq!(
        first_boot.status.code(),
        Some(0),
        "first-boot did not mint the key set"
    );

    // AC-2, second half: behind the migrations, `serve` refuses. Exit 2 is
    // `rahi_types::error::EXIT_STALE`; the chassis owns the code and this
    // asserts the cell inherits it.
    let stale = aicortex("serve", data_dir.path(), ports)?;
    let complaint = String::from_utf8_lossy(&stale.stderr);
    assert_eq!(
        stale.status.code(),
        Some(2),
        "serve against an unmigrated store exited {:?}:\n{complaint}",
        stale.status.code()
    );
    assert!(
        complaint.contains("stale") && complaint.contains("migrate"),
        "the refusal does not say what to run:\n{complaint}"
    );
    assert!(
        complaint.contains(&aicortex_store::EXPECTED_SCHEMA_VERSION.to_string()),
        "the refusal does not name the version the cell expects:\n{complaint}"
    );

    // AC-2, first half: from empty to the version the cell declares.
    let migrated = aicortex("migrate", data_dir.path(), ports)?;
    let report = String::from_utf8_lossy(&migrated.stdout);
    assert_eq!(
        migrated.status.code(),
        Some(0),
        "migrate failed:\n{}",
        String::from_utf8_lossy(&migrated.stderr)
    );
    assert!(
        report.contains(&format!(
            "schema_version 0 -> {}",
            aicortex_store::EXPECTED_SCHEMA_VERSION
        )),
        "migrate did not report reaching the expected version:\n{report}"
    );
    for migration in aicortex_store::migrations() {
        assert!(
            report.contains(&migration.version.to_string()),
            "migrate did not apply {} ({}):\n{report}",
            migration.version,
            migration.name
        );
    }

    // B-1: running it twice is a no-op.
    let again = aicortex("migrate", data_dir.path(), ports)?;
    let report = String::from_utf8_lossy(&again.stdout);
    assert_eq!(again.status.code(), Some(0));
    assert!(
        report.contains("applied: none"),
        "the second migrate applied something:\n{report}"
    );
    Ok(())
}
