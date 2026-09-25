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
//!
//! Spec 046 AC-3 adds the rahi 0.2.0 contract through the same binary:
//! `migrate` records each migration's checksum and declared `additive` flag
//! (FR-002), and `serve` starts against a store one additive migration ahead
//! of it but refuses, naming the version, one non-additive migration ahead
//! (FR-003). A store ahead of the binary is what a rollback produces, so the
//! test writes the extra migrations in-process through the chassis's own
//! store API with the deployment's key set, then hands the directory back to
//! the binary.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use rahi_store::{Migration, Store};

type Outcome = Result<(), String>;

/// A port the OS is not using, released before the child takes it.
fn free_port() -> Result<u16, String> {
    let listener = TcpListener::bind("127.0.0.1:0").map_err(|err| err.to_string())?;
    listener
        .local_addr()
        .map(|addr| addr.port())
        .map_err(|err| err.to_string())
}

/// Every `RAHI_*` the cell needs for `data_dir` on `ports`.
fn cell_env(data_dir: &Path, ports: (u16, u16, u16)) -> Vec<(&'static str, String)> {
    let (app, hiqlite_api, hiqlite_raft) = ports;
    vec![
        ("RAHI_PUBLIC_URL", format!("http://127.0.0.1:{app}")),
        ("RAHI_LISTEN_ADDR", format!("127.0.0.1:{app}")),
        ("RAHI_DATA_DIR", data_dir.display().to_string()),
        ("RAHI_HIQLITE_API_ADDR", format!("127.0.0.1:{hiqlite_api}")),
        (
            "RAHI_HIQLITE_RAFT_ADDR",
            format!("127.0.0.1:{hiqlite_raft}"),
        ),
        ("RAHI_RAUTHY_MODE", "none".to_owned()),
    ]
}

/// The command for `verb` against `data_dir`, with nothing else inherited.
fn command(verb: &str, data_dir: &Path, ports: (u16, u16, u16)) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_aicortex"));
    command.arg(verb).env_clear();
    for (key, value) in cell_env(data_dir, ports) {
        command.env(key, value);
    }
    command
}

/// Run a verb against `data_dir` with every `RAHI_*` the cell needs.
fn aicortex(verb: &str, data_dir: &Path, ports: (u16, u16, u16)) -> Result<Output, String> {
    command(verb, data_dir, ports)
        .output()
        .map_err(|err| format!("aicortex {verb} did not run: {err}"))
}

/// Open the deployment's store in-process, as the binary opens it: the same
/// configuration, the same key set, the same ports.
async fn open_store(data_dir: &Path, ports: (u16, u16, u16)) -> Result<Store, String> {
    let env: BTreeMap<String, String> = cell_env(data_dir, ports)
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value))
        .collect();
    let config = rahi_types::Config::from_env(&env).map_err(|err| err.to_string())?;
    let secrets = rahi_ops::KeySet::of(&config)
        .store_secrets()
        .map_err(|err| err.to_string())?;
    let store_config =
        rahi_ops::store_config(&config, &env, secrets).map_err(|err| err.to_string())?;
    Store::open(&store_config)
        .await
        .map_err(|err| err.to_string())
}

/// Apply `extra` on top of the binary's migrations, then stop the node.
async fn migrate_ahead(
    data_dir: &Path,
    ports: (u16, u16, u16),
    extra: Migration,
) -> Result<(), String> {
    let store = open_store(data_dir, ports).await?;
    let mut list = aicortex_store::migrations().to_vec();
    list.push(extra);
    let applied = store.handle().migrate(&list).await;
    store.shutdown().await.map_err(|err| err.to_string())?;
    applied.map(|_| ()).map_err(|err| err.to_string())
}

/// `GET /healthz` on `port`, true on a 200.
fn healthy(port: u16) -> bool {
    let Ok(mut stream) = TcpStream::connect(("127.0.0.1", port)) else {
        return false;
    };
    let request =
        format!("GET /healthz HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n");
    if stream.write_all(request.as_bytes()).is_err() {
        return false;
    }
    let mut response = String::new();
    let _ = stream.read_to_string(&mut response);
    response.starts_with("HTTP/1.1 200")
}

/// Stops the child on every exit path, including a failed assertion.
struct Serving(Child);

impl Serving {
    /// SIGTERM, then wait: an operator's stop, which lets the node release
    /// its lock so the next open of the directory succeeds. A kill would
    /// leave hiqlite's lock file behind.
    fn stop(mut self) -> Result<(), String> {
        let terminated = Command::new("kill")
            .args(["-TERM", &self.0.id().to_string()])
            .status()
            .map_err(|err| format!("kill did not run: {err}"))?;
        if !terminated.success() {
            return Err("SIGTERM was not delivered to serve".to_owned());
        }
        let deadline = Instant::now() + Duration::from_secs(60);
        while self.0.try_wait().map_err(|err| err.to_string())?.is_none() {
            if Instant::now() > deadline {
                return Err("serve did not stop within 60s of SIGTERM".to_owned());
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        Ok(())
    }
}

impl Drop for Serving {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
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

#[test]
fn fr002_fr003_migrate_records_the_contract_and_serve_crosses_only_additive_versions() -> Outcome {
    let data_dir = tempfile::tempdir().map_err(|err| err.to_string())?;
    let ports = (free_port()?, free_port()?, free_port()?);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|err| err.to_string())?;

    let first_boot = aicortex("first-boot", data_dir.path(), ports)?;
    assert_eq!(first_boot.status.code(), Some(0), "first-boot failed");
    let migrated = aicortex("migrate", data_dir.path(), ports)?;
    assert_eq!(
        migrated.status.code(),
        Some(0),
        "migrate failed:\n{}",
        String::from_utf8_lossy(&migrated.stderr)
    );

    // FR-002: one row per migration, with the binary's checksum and the
    // additive flag it declared. Every shipped migration is additive (046
    // B-2, D-2) except spec 014's fingerprint-version migration, which 046
    // B-2 names as not additive and 014 D-14 leaves undeclared.
    let recorded = runtime.block_on(async {
        let store = open_store(data_dir.path(), ports).await?;
        let rows = store.handle().recorded_migrations().await;
        store.shutdown().await.map_err(|err| err.to_string())?;
        rows.map_err(|err| err.to_string())
    })?;
    // Version 0 is the chassis's own baseline row, not a migration.
    let recorded: Vec<_> = recorded.into_iter().filter(|row| row.version > 0).collect();
    let shipped = aicortex_store::migrations();
    assert_eq!(recorded.len(), shipped.len(), "recorded: {recorded:#?}");
    for (row, migration) in recorded.iter().zip(shipped) {
        assert_eq!(row.version, migration.version);
        assert_eq!(
            row.checksum.as_deref(),
            Some(migration.checksum().as_str()),
            "version {} recorded another checksum",
            migration.version
        );
        let expected = migration.version != aicortex_store::migrations::ERASURE_RECEIPTS_VERSION;
        assert_eq!(
            migration.additive, expected,
            "version {} declares the wrong additive flag",
            migration.version
        );
        assert_eq!(
            row.additive,
            Some(expected),
            "version {}",
            migration.version
        );
    }

    // FR-003, first half: one additive migration ahead, `serve` starts.
    let last = aicortex_store::EXPECTED_SCHEMA_VERSION;
    let additive = Migration::new(
        last + 1,
        "fr003 additive ahead",
        "CREATE TABLE IF NOT EXISTS fr003_additive (x INTEGER)",
    )
    .additive();
    runtime.block_on(migrate_ahead(data_dir.path(), ports, additive))?;
    let mut serving = Serving(
        command("serve", data_dir.path(), ports)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|err| format!("serve did not start: {err}"))?,
    );
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if healthy(ports.0) {
            break;
        }
        if let Some(status) = serving.0.try_wait().map_err(|err| err.to_string())? {
            return Err(format!(
                "serve exited {status} against a store one additive migration ahead"
            ));
        }
        if Instant::now() > deadline {
            return Err("serve never answered /healthz".to_owned());
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    serving.stop()?;

    // FR-003, second half: one non-additive migration above that, and
    // `serve` refuses with the stale exit code, naming the version.
    let blocker = last + 2;
    let breaking = Migration::new(
        blocker,
        "fr003 non-additive ahead",
        "CREATE TABLE IF NOT EXISTS fr003_breaking (x INTEGER)",
    );
    runtime.block_on(migrate_ahead(data_dir.path(), ports, breaking))?;
    let refused = aicortex("serve", data_dir.path(), ports)?;
    let complaint = String::from_utf8_lossy(&refused.stderr);
    assert_eq!(
        refused.status.code(),
        Some(2),
        "serve one non-additive migration ahead exited {:?}:\n{complaint}",
        refused.status.code()
    );
    assert!(
        complaint.contains(&format!("version {blocker}")),
        "the refusal does not name version {blocker}:\n{complaint}"
    );
    Ok(())
}
