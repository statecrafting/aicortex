//! The seam, checked (spec 010).
//!
//! - B-2, FR-004, FR-005: all nine rahi crates pinned to one exact registry
//!   version, with no `[patch]`, `path`, or `git` anywhere.
//! - B-4, FR-003: the manifest parses, hashes, and is closed; the schema
//!   version is 0.
//! - FR-002: the binary lends the chassis's verbs without declaring any.
//! - FR-003, AC-3: preflight's report on an empty data directory.
//! - B-8, FR-004: nothing reimplements the router, the store, or the
//!   egress client.
//!
//! The pin and grep checks are functions over text, so each refusal is
//! tested as well as each pass: every check runs once over this repository
//! and once over a synthetic input carrying exactly the defect it exists to
//! catch.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use aicortex::Aicortex;
use rahi_cli::Cell;
use rahi_kernel::Manifest;
use rahi_types::MANIFEST_SCHEMA_VERSION;

type Outcome = Result<(), String>;

/// The chassis crates of B-2, all nine.
const RAHI_CRATES: [&str; 9] = [
    "rahi-types",
    "rahi-store",
    "rahi-ledger",
    "rahi-kernel",
    "rahi-idp",
    "rahi-edge",
    "rahi-ops",
    "rahi-harness",
    "rahi-cli",
];

/// Keys that take a dependency from somewhere other than the registry.
const NON_REGISTRY_KEYS: [&str; 5] = ["path", "git", "branch", "tag", "rev"];

/// The dependency tables a crate manifest may declare.
const DEPENDENCY_TABLES: [&str; 3] = ["dependencies", "dev-dependencies", "build-dependencies"];

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|err| format!("{}: {err}", path.display()))
}

fn parse_toml(text: &str, what: &str) -> Result<toml::Table, String> {
    text.parse::<toml::Table>()
        .map_err(|err| format!("{what} does not parse: {err}"))
}

/// `=MAJOR.MINOR.PATCH`, and nothing looser.
fn is_exact(requirement: &str) -> bool {
    let Some(version) = requirement.strip_prefix('=') else {
        return false;
    };
    let parts: Vec<&str> = version.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
}

/// B-2 over the workspace root manifest: every problem with the rahi pin,
/// empty when it holds.
fn root_pin_problems(manifest: &str) -> Result<Vec<String>, String> {
    let doc = parse_toml(manifest, "the workspace manifest")?;
    let mut problems = Vec::new();
    if doc.contains_key("patch") {
        problems.push("a [patch] table is present".to_owned());
    }
    let Some(deps) = doc
        .get("workspace")
        .and_then(|workspace| workspace.get("dependencies"))
        .and_then(toml::Value::as_table)
    else {
        problems.push("[workspace.dependencies] is missing".to_owned());
        return Ok(problems);
    };
    for name in deps.keys().filter(|name| name.starts_with("rahi")) {
        if !RAHI_CRATES.contains(&name.as_str()) {
            problems.push(format!("{name} is not one of the nine chassis crates"));
        }
    }
    let mut versions = BTreeSet::new();
    for name in RAHI_CRATES {
        let requirement = match deps.get(name) {
            None => {
                problems.push(format!("{name} is not declared"));
                continue;
            }
            Some(toml::Value::String(requirement)) => Some(requirement.as_str()),
            Some(toml::Value::Table(table)) => {
                for key in NON_REGISTRY_KEYS {
                    if table.contains_key(key) {
                        problems.push(format!("{name} carries a `{key}` key"));
                    }
                }
                table.get("version").and_then(toml::Value::as_str)
            }
            Some(_) => {
                problems.push(format!("{name} is neither a version nor a table"));
                continue;
            }
        };
        match requirement {
            Some(requirement) if is_exact(requirement) => {
                versions.insert(requirement.to_owned());
            }
            Some(requirement) => {
                problems.push(format!("{name} is pinned `{requirement}`, not `=x.y.z`"));
            }
            None => problems.push(format!("{name} has no version")),
        }
    }
    if versions.len() > 1 {
        problems.push(format!(
            "the rahi crates are pinned to more than one version: {versions:?}"
        ));
    }
    Ok(problems)
}

/// B-1 and B-2 over one crate manifest: a rahi dependency inherits the
/// workspace pin and says nothing else about where it comes from.
fn member_pin_problems(manifest: &str, what: &str) -> Result<Vec<String>, String> {
    let doc = parse_toml(manifest, what)?;
    let mut problems = Vec::new();
    if doc.contains_key("patch") {
        problems.push(format!("{what}: a [patch] table is present"));
    }
    for table_name in DEPENDENCY_TABLES {
        let Some(table) = doc.get(table_name).and_then(toml::Value::as_table) else {
            continue;
        };
        for (name, spec) in table.iter().filter(|(name, _)| name.starts_with("rahi")) {
            let inherits = spec
                .as_table()
                .and_then(|spec| spec.get("workspace"))
                .and_then(toml::Value::as_bool)
                == Some(true);
            if !inherits {
                problems.push(format!(
                    "{what}: [{table_name}] {name} does not inherit `workspace = true`"
                ));
            }
            if let Some(spec) = spec.as_table() {
                for key in NON_REGISTRY_KEYS.iter().copied().chain(["version"]) {
                    if spec.contains_key(key) {
                        problems.push(format!(
                            "{what}: [{table_name}] {name} carries a `{key}` key"
                        ));
                    }
                }
            }
        }
    }
    Ok(problems)
}

/// Every crate manifest in the workspace, by its path from the root.
fn member_manifests(root: &Path) -> Result<Vec<(String, String)>, String> {
    let mut manifests = Vec::new();
    for group in ["apps", "crates"] {
        let dir = root.join(group);
        if !dir.is_dir() {
            continue;
        }
        let entries = std::fs::read_dir(&dir).map_err(|err| format!("{group}: {err}"))?;
        for entry in entries {
            let entry = entry.map_err(|err| format!("{group}: {err}"))?;
            let manifest = entry.path().join("Cargo.toml");
            if manifest.is_file() {
                let name = format!("{group}/{}/Cargo.toml", entry.file_name().to_string_lossy());
                manifests.push((name, read(&manifest)?));
            }
        }
    }
    manifests.sort();
    Ok(manifests)
}

/// A workspace manifest with a correct pin, for the refusal cases to break.
fn synthetic_root(version_of: impl Fn(&str) -> String) -> String {
    let mut text =
        String::from("[workspace]\nmembers = [\"apps/*\"]\n\n[workspace.dependencies]\n");
    for name in RAHI_CRATES {
        text.push_str(&format!("{name} = {}\n", version_of(name)));
    }
    text
}

#[test]
fn b2_the_workspace_pins_nine_rahi_crates_to_one_exact_version() -> Outcome {
    let root = workspace_root();
    let mut problems = root_pin_problems(&read(&root.join("Cargo.toml"))?)?;
    for (name, manifest) in member_manifests(&root)? {
        problems.extend(member_pin_problems(&manifest, &name)?);
    }
    for config in [".cargo/config.toml", ".cargo/config"] {
        let path = root.join(config);
        if path.is_file() && parse_toml(&read(&path)?, config)?.contains_key("patch") {
            problems.push(format!("{config}: a [patch] table is present"));
        }
    }
    assert!(problems.is_empty(), "B-2 does not hold: {problems:#?}");
    Ok(())
}

#[test]
fn fr004_fr005_the_pin_check_refuses_each_defect() -> Outcome {
    let good = synthetic_root(|_| "\"=1.2.3\"".to_owned());
    assert_eq!(root_pin_problems(&good)?, Vec::<String>::new());

    let cases: Vec<(&str, String)> = vec![
        (
            "a [patch] table",
            format!("{good}\n[patch.crates-io]\nrahi-types = {{ path = \"../rahi\" }}\n"),
        ),
        (
            "a missing crate",
            good.replace("rahi-cli = \"=1.2.3\"\n", ""),
        ),
        (
            "a path key",
            synthetic_root(|name| {
                if name == "rahi-store" {
                    "{ version = \"=1.2.3\", path = \"../rahi/crates/rahi-store\" }".to_owned()
                } else {
                    "\"=1.2.3\"".to_owned()
                }
            }),
        ),
        (
            "a git key",
            synthetic_root(|name| {
                if name == "rahi-edge" {
                    "{ git = \"https://example.invalid/rahi\", rev = \"444bcf8\" }".to_owned()
                } else {
                    "\"=1.2.3\"".to_owned()
                }
            }),
        ),
        (
            "a split version",
            synthetic_root(|name| {
                if name == "rahi-ops" {
                    "\"=1.2.4\"".to_owned()
                } else {
                    "\"=1.2.3\"".to_owned()
                }
            }),
        ),
        (
            "a loose requirement",
            synthetic_root(|name| {
                if name == "rahi-kernel" {
                    "\"1.2.3\"".to_owned()
                } else {
                    "\"=1.2.3\"".to_owned()
                }
            }),
        ),
    ];
    for (defect, manifest) in cases {
        let problems = root_pin_problems(&manifest)?;
        assert!(!problems.is_empty(), "the pin check accepted {defect}");
    }

    let member_ok = "[package]\nname = \"x\"\n\n[dependencies]\nrahi-cli = { workspace = true }\n";
    assert_eq!(
        member_pin_problems(member_ok, "member")?,
        Vec::<String>::new()
    );
    let member_path =
        "[package]\nname = \"x\"\n\n[dev-dependencies]\nrahi-ops = { path = \"../rahi\" }\n";
    assert!(!member_pin_problems(member_path, "member")?.is_empty());
    Ok(())
}

#[test]
fn b4_fr003_the_manifest_is_closed_and_the_schema_version_is_zero() -> Outcome {
    let manifest = Manifest::parse(Aicortex::manifest()).map_err(|err| err.to_string())?;
    let again = Manifest::parse(Aicortex::manifest()).map_err(|err| err.to_string())?;
    let hash = manifest.hash().map_err(|err| err.to_string())?;
    assert_eq!(hash, again.hash().map_err(|err| err.to_string())?);

    assert_eq!(
        manifest.schema_version.as_deref(),
        Some(MANIFEST_SCHEMA_VERSION),
        "B-4: the ceiling names the schema the pinned chassis speaks"
    );
    assert_eq!(manifest.app.name.as_str(), "aicortex");
    assert!(
        manifest.resources.egress.is_empty(),
        "B-4: egress starts empty"
    );
    assert!(manifest.resources.tables.is_empty());
    assert!(manifest.resources.kv.is_empty());
    assert!(manifest.resources.counters.is_empty());
    assert!(manifest.resources.secrets.is_empty());
    assert!(manifest.capabilities.is_empty());
    assert!(manifest.services.is_empty());
    assert!(
        Aicortex::migrations().is_empty(),
        "no crate owns schema yet"
    );
    Ok(())
}

fn aicortex(args: &[&str], env: &[(&str, &Path)]) -> Result<Output, String> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_aicortex"));
    command.args(args).env_clear();
    for (key, value) in env {
        command.env(key, value);
    }
    command
        .output()
        .map_err(|err| format!("aicortex {args:?} did not run: {err}"))
}

#[test]
fn fr002_help_lists_the_chassis_verbs() -> Outcome {
    let output = aicortex(&["--help"], &[])?;
    assert!(
        output.status.success(),
        "--help exited {:?}",
        output.status.code()
    );
    let usage = String::from_utf8_lossy(&output.stdout);
    for verb in [
        "serve",
        "preflight",
        "migrate",
        "backup",
        "restore",
        "ledger verify",
    ] {
        assert!(
            usage.contains(verb),
            "--help does not list `{verb}`:\n{usage}"
        );
    }
    Ok(())
}

#[test]
fn fr003_ac3_preflight_on_an_empty_data_directory_fails_only_keys() -> Outcome {
    let data_dir = tempfile::tempdir().map_err(|err| err.to_string())?;
    let output = aicortex(
        &["preflight"],
        &[
            ("RAHI_PUBLIC_URL", Path::new("http://127.0.0.1:9")),
            ("RAHI_DATA_DIR", data_dir.path()),
        ],
    )?;
    let report = String::from_utf8_lossy(&output.stdout);
    assert_eq!(output.status.code(), Some(1), "preflight report:\n{report}");

    let checks: Vec<(&str, &str)> = report
        .lines()
        .filter_map(|line| {
            let (verdict, rest) = line.split_once(' ')?;
            let (name, _) = rest.split_once(':')?;
            matches!(verdict, "PASS" | "FAIL" | "SKIP").then_some((verdict, name))
        })
        .collect();
    let names: Vec<&str> = checks.iter().map(|(_, name)| *name).collect();
    assert_eq!(
        names,
        rahi_ops::preflight::CHECKS.to_vec(),
        "report:\n{report}"
    );

    let failed: Vec<&str> = checks
        .iter()
        .filter(|(verdict, _)| *verdict == "FAIL")
        .map(|(_, name)| *name)
        .collect();
    assert_eq!(failed, vec!["keys"], "report:\n{report}");

    // Nothing that listens was opened: the store and rauthy were skipped.
    for name in ["hiqlite", "rauthy"] {
        assert!(
            checks.contains(&("SKIP", name)),
            "{name} was not skipped:\n{report}"
        );
    }
    Ok(())
}

// B-8. The needles are assembled with `concat!` so this file does not match
// itself when it scans the workspace.
const ROUTER_NEW: &str = concat!("Router", "::new(");
const HIQLITE_PATH: &str = concat!("hiqlite", "::");
const RAW_CLIENTS: [&str; 3] = [
    concat!("reqwest::Client", "::new("),
    concat!("reqwest::Client", "::builder("),
    concat!("reqwest::ClientBuilder", "::new("),
];

/// Where a router may be built: the cell, and the surface crates of 020
/// and 021.
const ROUTER_HOMES: [&str; 3] = [
    "apps/aicortex/src/cell.rs",
    "crates/aicortex-api/",
    "crates/aicortex-mcp/",
];

/// The governed egress call sites. None exists before a spec declares an
/// egress host (B-4).
const EGRESS_SITES: [&str; 0] = [];

/// B-8 over one source file: every line that reimplements the chassis.
fn reimplementation_problems(path: &str, source: &str) -> Vec<String> {
    let router_home = ROUTER_HOMES.iter().any(|home| path.starts_with(home));
    let egress_site = EGRESS_SITES.iter().any(|site| path.starts_with(site));
    let mut problems = Vec::new();
    for (number, line) in source.lines().enumerate() {
        let code = line.trim_start();
        if code.starts_with("//") {
            continue;
        }
        let at = number + 1;
        if code.contains(ROUTER_NEW) && !router_home {
            problems.push(format!(
                "{path}:{at}: a router is built outside the cell and the surfaces"
            ));
        }
        if code.contains(HIQLITE_PATH) {
            problems.push(format!("{path}:{at}: hiqlite is used directly"));
        }
        if RAW_CLIENTS.iter().any(|needle| code.contains(needle)) && !egress_site {
            problems.push(format!(
                "{path}:{at}: an HTTP client is built outside governed egress"
            ));
        }
    }
    problems
}

/// Every Rust source under `apps/` and `crates/`, by its path from the root.
fn rust_sources(root: &Path) -> Result<Vec<(String, String)>, String> {
    let mut pending: Vec<PathBuf> = ["apps", "crates"]
        .iter()
        .map(|group| root.join(group))
        .filter(|dir| dir.is_dir())
        .collect();
    let mut sources = Vec::new();
    while let Some(dir) = pending.pop() {
        let entries = std::fs::read_dir(&dir).map_err(|err| format!("{}: {err}", dir.display()))?;
        for entry in entries {
            let path = entry.map_err(|err| err.to_string())?.path();
            if path.is_dir() {
                if path.file_name().is_some_and(|name| name != "target") {
                    pending.push(path);
                }
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                let relative = path
                    .strip_prefix(root)
                    .map_err(|err| err.to_string())?
                    .to_string_lossy()
                    .replace('\\', "/");
                sources.push((relative, read(&path)?));
            }
        }
    }
    sources.sort();
    Ok(sources)
}

#[test]
fn b8_nothing_reimplements_the_chassis() -> Outcome {
    let sources = rust_sources(&workspace_root())?;
    assert!(
        sources
            .iter()
            .any(|(path, _)| path == "apps/aicortex/src/cell.rs"),
        "the scan did not find the cell, so it scanned nothing"
    );
    let problems: Vec<String> = sources
        .iter()
        .flat_map(|(path, source)| reimplementation_problems(path, source))
        .collect();
    assert!(problems.is_empty(), "B-8 does not hold: {problems:#?}");
    Ok(())
}

#[test]
fn fr004_the_grep_refuses_a_stray_router_the_store_and_a_raw_client() {
    let router = format!("fn stray() {{ let _ = axum::{ROUTER_NEW}); }}");
    assert!(reimplementation_problems("apps/aicortex/src/cell.rs", &router).is_empty());
    assert!(reimplementation_problems("crates/aicortex-api/src/router.rs", &router).is_empty());
    assert!(!reimplementation_problems("apps/aicortex/src/main.rs", &router).is_empty());
    assert!(!reimplementation_problems("crates/aicortex-store/src/lib.rs", &router).is_empty());

    let store = format!("use {HIQLITE_PATH}Client;");
    assert!(!reimplementation_problems("apps/aicortex/src/cell.rs", &store).is_empty());

    for needle in RAW_CLIENTS {
        let client = format!("let client = {needle});");
        assert!(!reimplementation_problems("crates/aicortex-embed/src/lib.rs", &client).is_empty());
    }

    let comment = format!("// {HIQLITE_PATH} is the chassis's, never ours");
    assert!(reimplementation_problems("apps/aicortex/src/cell.rs", &comment).is_empty());
}
