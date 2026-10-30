//! The embedding pipeline through the real binary (spec 015 B-3, B-6, AC-2,
//! FR-005).
//!
//! The application seams are tested in `embedding.rs`. This file asks the
//! process-boundary questions of the specification:
//!
//! - `aicortex preflight` prints `app.embedding` with the AC-2 report, and a
//!   remote provider missing from the manifest ceiling fails it with the
//!   named capability error (FR-005).
//! - `aicortex serve` refuses to start for the same configuration, at boot
//!   and not at first use (B-6).
//!
//! The chassis skips application checks when the store did not open, so the
//! sequence starts with `first-boot` and `migrate`, as a deployment does.

use std::net::TcpListener;
use std::path::Path;
use std::process::{Command, Output};

type Outcome = Result<(), String>;

const REMOTE: [(&str, &str); 4] = [
    ("AICORTEX_EMBED_PROVIDER", "remote"),
    ("AICORTEX_EMBED_MODEL", "remote-model"),
    ("AICORTEX_EMBED_DIMS", "8"),
    ("AICORTEX_EMBED_ENDPOINT", "https://models.example/v1/embed"),
];

fn free_port() -> Result<u16, String> {
    let listener = TcpListener::bind("127.0.0.1:0").map_err(|err| err.to_string())?;
    listener
        .local_addr()
        .map(|addr| addr.port())
        .map_err(|err| err.to_string())
}

/// The deployment's loopback ports: the app, the hiqlite API, hiqlite Raft.
type Ports = (u16, u16, u16);

/// How many fresh deployments a scenario gets before a port race fails it.
const ATTEMPTS: usize = 5;

/// What a bind reports when another socket holds the port.
const LOST_PORT: &str = "Address already in use";

/// Run `scenario` against a fresh data directory on fresh ports.
///
/// `free_port` releases its probe listener before the child binds, so under
/// a parallel run another test can take a port in between; the ports are
/// then persisted in the deployment, so the whole scenario starts over in a
/// fresh directory on fresh ports, a bounded number of times. Only an error
/// naming the collision is retried (`run` turns a lost port into one); any
/// other outcome is the test's.
fn on_fresh_ports(scenario: impl Fn(&Path, Ports) -> Outcome) -> Outcome {
    let mut last = String::new();
    for _ in 0..ATTEMPTS {
        let data_dir = tempfile::tempdir().map_err(|err| err.to_string())?;
        let ports = (free_port()?, free_port()?, free_port()?);
        match scenario(data_dir.path(), ports) {
            Err(error) if error.contains(LOST_PORT) => last = error,
            outcome => return outcome,
        }
    }
    Err(format!(
        "the scenario lost a port in each of {ATTEMPTS} fresh deployments: {last}"
    ))
}

fn run(
    verb: &str,
    data_dir: &Path,
    ports: (u16, u16, u16),
    extra: &[(&str, &str)],
) -> Result<Output, String> {
    let (app, api, raft) = ports;
    let mut command = Command::new(env!("CARGO_BIN_EXE_aicortex"));
    command.arg(verb).env_clear();
    for (key, value) in [
        ("RAHI_PUBLIC_URL", format!("http://127.0.0.1:{app}")),
        ("RAHI_LISTEN_ADDR", format!("127.0.0.1:{app}")),
        ("RAHI_DATA_DIR", data_dir.display().to_string()),
        ("RAHI_HIQLITE_API_ADDR", format!("127.0.0.1:{api}")),
        ("RAHI_HIQLITE_RAFT_ADDR", format!("127.0.0.1:{raft}")),
        ("RAHI_RAUTHY_MODE", "none".to_owned()),
    ] {
        command.env(key, value);
    }
    for (key, value) in extra {
        command.env(key, value);
    }
    let output = command
        .output()
        .map_err(|err| format!("aicortex {verb} did not run: {err}"))?;
    // A run that lost one of its ports is an error naming the collision, for
    // `on_fresh_ports` to retry, rather than an outcome to assert about.
    let stderr = String::from_utf8_lossy(&output.stderr);
    if stderr.contains(LOST_PORT) {
        return Err(format!("aicortex {verb} lost a port: {stderr}"));
    }
    Ok(output)
}

fn prepared(data_dir: &Path, ports: (u16, u16, u16)) -> Outcome {
    for verb in ["first-boot", "migrate"] {
        let output = run(verb, data_dir, ports, &[])?;
        if output.status.code() != Some(0) {
            return Err(format!(
                "{verb} exited {:?}:\n{}",
                output.status.code(),
                String::from_utf8_lossy(&output.stderr)
            ));
        }
    }
    Ok(())
}

fn app_embedding_lines(stdout: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut inside = false;
    for line in stdout.lines() {
        if line.contains("app.embedding") {
            inside = true;
            lines.push(line);
        } else if inside && line.starts_with("  | ") {
            lines.push(line);
        } else {
            inside = false;
        }
    }
    lines
}

#[test]
fn ac2_preflight_prints_the_embedding_report_on_a_migrated_deployment() -> Outcome {
    on_fresh_ports(preflight_prints_the_embedding_report)
}

fn preflight_prints_the_embedding_report(data_dir: &Path, ports: Ports) -> Outcome {
    prepared(data_dir, ports)?;

    let output = run("preflight", data_dir, ports, &[])?;
    let report = String::from_utf8_lossy(&output.stdout);
    let lines = app_embedding_lines(&report);
    assert!(
        lines
            .first()
            .is_some_and(|line| line.starts_with("PASS app.embedding")),
        "report:\n{report}"
    );
    for needle in [
        "active model: none",
        "queue: pending=0 dead=0 quarantined=0",
        "scopes read: 0",
    ] {
        assert!(
            lines.iter().any(|line| line.contains(needle)),
            "{needle} missing:\n{report}"
        );
    }
    Ok(())
}

#[test]
fn fr005_preflight_and_serve_refuse_a_remote_provider_absent_from_the_ceiling() -> Outcome {
    on_fresh_ports(preflight_and_serve_refuse_an_undeclared_remote)
}

fn preflight_and_serve_refuse_an_undeclared_remote(data_dir: &Path, ports: Ports) -> Outcome {
    prepared(data_dir, ports)?;

    let output = run("preflight", data_dir, ports, &REMOTE)?;
    let report = String::from_utf8_lossy(&output.stdout);
    assert_eq!(output.status.code(), Some(1), "report:\n{report}");
    let lines = app_embedding_lines(&report);
    assert!(
        lines
            .first()
            .is_some_and(|line| line.starts_with("FAIL app.embedding")
                && line.contains("embedding.remote.egress")
                && line.contains("models.example")),
        "report:\n{report}"
    );

    let served = run("serve", data_dir, ports, &REMOTE)?;
    let complaint = String::from_utf8_lossy(&served.stderr);
    assert_ne!(served.status.code(), Some(0), "serve started:\n{complaint}");
    assert!(
        complaint.contains("embedding.remote.egress"),
        "serve did not name the capability:\n{complaint}"
    );
    Ok(())
}
