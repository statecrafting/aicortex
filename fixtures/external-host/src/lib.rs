//! A separate Rahi cell that links Aicortex only through public libraries.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use axum::Router;
use rahi_cli::Cell;
use rahi_edge::AppState;
use rahi_store::{Migration, MigrationSet};

/// The external host used only by spec 053's composition fixture.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ExternalHost;

impl ExternalHost {
    /// A minimal valid cell manifest.
    pub const MANIFEST: &'static str = r#"
schema_version = "1.0.0"

[app]
name = "aicortex-external-host-fixture"
org = "statecrafting"

[ledger]
schema_version = "1.0.0"
max_record_bytes = 65536

[observability]
metrics_path = "/metrics"
otel = false

[auth]
operator_role = "rahi-operator"

[contract]
version = "1.0.0"
"#;
}

static HOST_MIGRATIONS: std::sync::LazyLock<[Migration; 1]> = std::sync::LazyLock::new(|| {
    [Migration::new(
        1,
        "external host projection revision",
        "CREATE TABLE host_projection_revision (
            revision_id TEXT PRIMARY KEY,
            subject_key TEXT NOT NULL,
            projection_digest TEXT NOT NULL,
            document TEXT NOT NULL
        )",
    )
    .additive()]
});

impl Cell for ExternalHost {
    fn manifest() -> &'static str {
        Self::MANIFEST
    }

    fn migrations() -> &'static [Migration] {
        HOST_MIGRATIONS.as_slice()
    }

    fn migration_sets() -> Vec<MigrationSet> {
        #[allow(
            clippy::expect_used,
            reason = "the set is built only from fixed literals"
        )]
        let aicortex = aicortex_store::migration_set()
            .expect("the fixed Aicortex migration-set contract is valid");
        vec![
            rahi_store::coordination_set(),
            rahi_store::receipt_set(),
            aicortex,
        ]
    }

    fn routes(state: AppState) -> Router {
        let _ = state;
        Router::new()
    }
}
