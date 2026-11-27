//! The ingest preflight check at the application boundary (spec 030 AC-2,
//! FR-004, D-7).
//!
//! The process-level report is `embedding_process.rs`'s; this file holds the
//! check to the cases a fresh deployment cannot reach: a registered source and
//! its hosts, a ceiling that no longer covers one, and a policy typo.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use aicortex::ingest_preflight;
use aicortex_ingest::{
    AdapterContext, AdapterError, AdapterResult, BoxFuture, Candidate, Cursor, INGEST_SERVICE,
    Page, Probe, RawItem, Registry, SourceAdapter, SourceId, SourceKind,
};
use aicortex_store::ReadStore;
use rahi_cli::AppOutcome;
use rahi_kernel::{Manifest, ServiceName};
use rahi_store::Value;
use rahi_types::Error;
use serde::de::DeserializeOwned;

/// A store that holds no run: every read is empty.
struct Empty;

impl ReadStore for Empty {
    async fn query<T: DeserializeOwned + Send + 'static>(
        &self,
        _sql: &'static str,
        _values: Vec<Value>,
    ) -> Result<Vec<T>, Error> {
        Ok(Vec::new())
    }

    async fn query_consistent<T: DeserializeOwned + Send + 'static>(
        &self,
        _sql: &'static str,
        _values: Vec<Value>,
    ) -> Result<Vec<T>, Error> {
        Ok(Vec::new())
    }
}

struct Reader {
    hosts: Vec<String>,
}

impl SourceAdapter for Reader {
    fn id(&self) -> SourceId {
        SourceId::new("pages").unwrap()
    }

    fn kind(&self) -> SourceKind {
        SourceKind::Public
    }

    fn hosts(&self) -> Vec<String> {
        self.hosts.clone()
    }

    fn probe<'a>(&'a self, _: &'a AdapterContext) -> BoxFuture<'a, AdapterResult<Probe>> {
        Box::pin(async { Err(AdapterError::Fatal("not used".to_owned())) })
    }

    fn page<'a>(
        &'a self,
        _: &'a AdapterContext,
        _: Option<&'a Cursor>,
    ) -> BoxFuture<'a, AdapterResult<Page>> {
        Box::pin(async { Err(AdapterError::Fatal("not used".to_owned())) })
    }

    fn map(&self, _: &RawItem) -> AdapterResult<Candidate> {
        Err(AdapterError::Fatal("not used".to_owned()))
    }
}

fn manifest(egress: &[&str]) -> Manifest {
    let hosts = egress
        .iter()
        .map(|host| format!("\"{host}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let grants: String = egress
        .iter()
        .enumerate()
        .map(|(index, host)| {
            format!(
                "\n[[capabilities]]\nid = \"ingest-egress-{index}\"\nkind = \"http.egress\"\n\
                 resource = \"{host}\"\n"
            )
        })
        .collect();
    let ids = (0..egress.len())
        .map(|index| format!("\"ingest-egress-{index}\""))
        .collect::<Vec<_>>()
        .join(", ");
    Manifest::parse(&format!(
        r#"
schema_version = "1.0.0"

[app]
name = "aicortex"
org = "statecrafting"

[resources]
egress = [{hosts}]
{grants}
[services.ingest]
capabilities = [{ids}]

[ledger]
schema_version = "1.0.0"
max_record_bytes = 65536

[observability]
metrics_path = "/metrics"
otel = false

[auth]
operator_role = "aicortex_operator"

[contract]
version = "0.1.0"
"#
    ))
    .expect("the manifest parses")
}

fn registry(granted: &Manifest, hosts: &[&str]) -> Registry {
    let mut registry = Registry::new();
    registry
        .register_under(
            Arc::new(Reader {
                hosts: hosts.iter().map(|host| (*host).to_owned()).collect(),
            }),
            granted,
            &ServiceName::parse(INGEST_SERVICE).unwrap(),
        )
        .expect("the source registers");
    registry
}

fn env(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect()
}

const BUDGET: Duration = Duration::from_secs(1);

#[tokio::test]
async fn ac2_the_check_lists_sources_and_their_hosts() {
    let granted = manifest(&["pages.example.org"]);
    let sources = registry(&granted, &["pages.example.org"]);
    let verdict = ingest_preflight::verdict(&env(&[]), &granted, &sources, &Empty, BUDGET).await;
    assert_eq!(verdict.outcome, AppOutcome::Pass, "{verdict:?}");
    assert!(
        verdict
            .detail
            .contains("1 source(s) registered, 0 scope(s) with runs, 0 failed"),
        "{verdict:?}"
    );
    assert!(
        verdict
            .report
            .contains(&"source pages: kind=public hosts=pages.example.org".to_owned()),
        "{verdict:?}"
    );

    let none =
        ingest_preflight::verdict(&env(&[]), &granted, &Registry::new(), &Empty, BUDGET).await;
    assert!(none.report.contains(&"registered sources: 0".to_owned()));
}

#[tokio::test]
async fn fr004_a_ceiling_narrowed_after_registration_fails_with_the_chassis_wording() {
    let granted = manifest(&["pages.example.org"]);
    let sources = registry(&granted, &["pages.example.org"]);
    let narrowed = manifest(&[]);
    let verdict = ingest_preflight::verdict(&env(&[]), &narrowed, &sources, &Empty, BUDGET).await;
    assert_eq!(verdict.outcome, AppOutcome::Fail, "{verdict:?}");
    assert_eq!(
        verdict.detail,
        "capability http.egress on pages.example.org for service ingest is not in the \
         manifest ceiling"
    );
    assert!(
        verdict
            .report
            .contains(&"host not granted: source pages reaches pages.example.org".to_owned()),
        "{verdict:?}"
    );
}

#[tokio::test]
async fn a_policy_that_names_neither_choice_fails_the_check() {
    let granted = manifest(&[]);
    for (value, outcome) in [
        ("quarantine", AppOutcome::Pass),
        ("admit", AppOutcome::Pass),
        ("maybe", AppOutcome::Fail),
    ] {
        let verdict = ingest_preflight::verdict(
            &env(&[("AICORTEX_INGEST_PUBLIC_ORIGIN", value)]),
            &granted,
            &Registry::new(),
            &Empty,
            BUDGET,
        )
        .await;
        assert_eq!(verdict.outcome, outcome, "{value}: {verdict:?}");
    }
}
