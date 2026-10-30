//! Scan latency at 1000, 20000, and 200000 chunks (spec 016 FR-007, AC-2).
//!
//! `cargo bench -p aicortex-index` seeds a real single-voter store at each
//! size, runs the scan repeatedly with no budget, and writes the latency
//! table spec 040 reads as its baseline: `aicortex-index/scan-latency.toml`
//! under the cargo target directory, or the path in
//! `AICORTEX_SCAN_LATENCY_OUT` (D-8). The table is also printed.
//!
//! `AICORTEX_BENCH_DIMS` overrides the vector width (default 384).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]
// Spec 010 B-1: a crate root of aicortex-index, which carries the one
// documented float allow; latencies are reported in fractional milliseconds.
#![allow(clippy::float_arithmetic)]

#[path = "../tests/common/mod.rs"]
mod common;

use std::fmt::Write as _;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use aicortex_index::{DEFAULT_BUDGET, Filter, ScanConfig, ScanQuery, scan};
use aicortex_types::Status;

const SIZES: [u64; 3] = [1_000, 20_000, 200_000];
const PER_MEMORY: u32 = 4;
const K: usize = 20;

struct Case {
    chunks: u64,
    runs: usize,
    p50: Duration,
    p95: Duration,
    max: Duration,
}

fn millis(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

/// Nearest-rank percentile of sorted samples.
fn percentile(sorted: &[Duration], pct: usize) -> Duration {
    let rank = (pct * sorted.len()).div_ceil(100).max(1);
    sorted[rank - 1]
}

/// Where the table goes: the override, or `<target>/aicortex-index/`.
fn output() -> PathBuf {
    if let Some(path) = std::env::var_os("AICORTEX_SCAN_LATENCY_OUT") {
        return PathBuf::from(path);
    }
    // The bench runs from `<target>/<profile>/deps/`.
    let exe = std::env::current_exe().unwrap();
    let target = exe
        .ancestors()
        .nth(3)
        .map_or_else(|| PathBuf::from("target"), PathBuf::from);
    target.join("aicortex-index").join("scan-latency.toml")
}

async fn measure(chunks: u64, dims: u16) -> Case {
    let node = common::Node::migrated().await;
    let store = node.handle();
    let (scope, scope_id) = common::scope("bench");
    let model = common::revision(1, dims);
    common::register_scope(&store, &scope).await;
    common::register_model(&store, &model).await;
    let rows = common::chunks(
        &scope_id,
        &model,
        1,
        chunks / u64::from(PER_MEMORY),
        PER_MEMORY,
        3,
    );
    common::seed(&store, &scope_id, &model, &rows, Status::Active).await;
    drop(rows);

    let filter = Filter::retrievable();
    let config = ScanConfig {
        budget: Duration::from_secs(3_600),
        ..ScanConfig::default()
    };
    let runs = if chunks >= 200_000 { 5 } else { 15 };
    let mut samples = Vec::with_capacity(runs);
    for run in 0..=runs {
        let needle = common::unit_vector(1_000 + run as u64, dims);
        let query = ScanQuery {
            scope: &scope_id,
            model: model.comparable(),
            vector: &needle,
            k: K,
            filter: &filter,
        };
        let started = Instant::now();
        let outcome = scan(&store, &query, config).await.unwrap();
        let elapsed = started.elapsed();
        assert_eq!(outcome.scanned, chunks);
        assert!(!outcome.truncated);
        // The first run warms the page cache and is not counted.
        if run > 0 {
            samples.push(elapsed);
        }
    }
    node.shutdown().await;
    samples.sort();
    Case {
        chunks,
        runs,
        p50: percentile(&samples, 50),
        p95: percentile(&samples, 95),
        max: *samples.last().unwrap(),
    }
}

fn main() {
    let dims: u16 = std::env::var("AICORTEX_BENCH_DIMS")
        .ok()
        .map_or(384, |value| {
            value.parse().expect("AICORTEX_BENCH_DIMS is a u16")
        });
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let mut table = String::new();
    writeln!(
        table,
        "# Scan latency for spec 016 (FR-007, AC-2), the baseline spec 040 reads.\n\
         dims = {dims}\npage_size = {}\nk = {K}\nbudget_ms = {}\nprofile = \"{}\"",
        ScanConfig::default().page_size,
        DEFAULT_BUDGET.as_millis(),
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        },
    )
    .unwrap();
    for chunks in SIZES {
        let case = runtime.block_on(measure(chunks, dims));
        writeln!(
            table,
            "\n[[case]]\nchunks = {}\nruns = {}\np50_ms = {:.3}\np95_ms = {:.3}\nmax_ms = {:.3}",
            case.chunks,
            case.runs,
            millis(case.p50),
            millis(case.p95),
            millis(case.max),
        )
        .unwrap();
        eprintln!(
            "scan {:>7} chunks: p50 {:>9.3} ms  p95 {:>9.3} ms",
            case.chunks,
            millis(case.p50),
            millis(case.p95)
        );
    }
    let path = output();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, &table).unwrap();
    println!("{table}");
    eprintln!("wrote {}", path.display());
}
