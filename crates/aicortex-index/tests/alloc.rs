//! FR-002: peak allocation during a scan is one page plus `k` candidates.
//!
//! Its own test binary, with a counting global allocator and a single test,
//! so nothing else in the process allocates while the scan is measured. The
//! pages come from a synthetic [`PageSource`] that builds each page afresh,
//! as the store does, so what is measured is the scan's own retention: if it
//! held on to rows, decoded vectors, or pages, the peak would grow with the
//! scope instead of staying flat (D-7).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
// Spec 010 B-1: a crate root of aicortex-index, which carries the one
// documented float allow.
#![allow(clippy::float_arithmetic)]

use std::future::Future;

use aicortex_index::{Comparable, Filter, PageSource, ScanConfig, ScanQuery, VectorRow, scan_rows};
use aicortex_store::ScopeId;
use aicortex_types::Scope;
use peak_alloc::PeakAlloc;
use rahi_store::Blob;
use rahi_types::{Error, Sub};

#[global_allocator]
static COUNTER: PeakAlloc = PeakAlloc;

const DIMS: u16 = 384;
const PAGE: u32 = 256;
const K: usize = 20;

/// `total` rows in key order, generated a page at a time.
struct Synthetic {
    total: u64,
}

fn row(n: u64) -> VectorRow {
    let mut state = n.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
    let mut bytes = Vec::with_capacity(usize::from(DIMS) * 4);
    for _ in 0..DIMS {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let value = (f64::from(u32::try_from(state >> 40).unwrap()) / f64::from(1_u32 << 24))
            .mul_add(2.0, -1.0) as f32
            / 20.0;
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    VectorRow {
        chunk_id: format!("chunk-{n:012}"),
        memory_id: format!("00000000-0000-7000-8000-{n:012x}"),
        chunk_ordinal: 0,
        dims: i64::from(DIMS),
        vector: Blob::new(bytes).unwrap(),
    }
}

impl PageSource for Synthetic {
    fn page(
        &mut self,
        after: Option<(&str, i64)>,
        size: u32,
    ) -> impl Future<Output = Result<Vec<VectorRow>, Error>> + Send {
        let start = after.map_or(0, |(memory, _)| {
            u64::from_str_radix(memory.rsplit('-').next().unwrap(), 16).unwrap() + 1
        });
        let end = (start + u64::from(size)).min(self.total);
        let page: Vec<VectorRow> = (start..end).map(row).collect();
        std::future::ready(Ok(page))
    }
}

/// The scan's peak above the allocation level it started at, in bytes.
fn peak_of(total: u64, query: &ScanQuery<'_>) -> (usize, u64) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let config = ScanConfig {
        page_size: PAGE,
        budget: std::time::Duration::from_secs(600),
    };
    let mut source = Synthetic { total };
    let base = COUNTER.current_usage();
    COUNTER.reset_peak_usage();
    let outcome = runtime
        .block_on(scan_rows(&mut source, query, config))
        .unwrap();
    let peak = COUNTER.peak_usage().saturating_sub(base);
    assert_eq!(outcome.candidates.len(), K);
    (peak, outcome.scanned)
}

#[test]
fn fr002_peak_allocation_is_one_page_plus_k_candidates() {
    let scope = ScopeId::of(&Scope::personal(Sub::new("alloc")));
    let model = Comparable {
        model_id: "alloc-model",
        revision: 1,
        dims: DIMS,
        normalized: false,
    };
    let filter = Filter::retrievable();
    let needle: Vec<f32> = (0..DIMS).map(|i| f32::from(i % 7) - 3.0).collect();
    let query = ScanQuery {
        scope: &scope,
        model,
        vector: &needle,
        k: K,
        filter: &filter,
    };

    // What one page of rows costs, measured the same way.
    let base = COUNTER.current_usage();
    let page: Vec<VectorRow> = (0..u64::from(PAGE)).map(row).collect();
    let one_page = COUNTER.current_usage() - base;
    drop(page);

    // What `k` retained candidates cost: each keeps its chunk id and memory
    // id strings plus a heap slot; a generous fixed allowance covers the
    // query copy, the decode buffer, the cursor, and the runtime's polling.
    let per_candidate = 64 + 2 * 48 + 32;
    let allowance = 2 * usize::from(DIMS) * 4 + 16 * 1024;
    let bound = one_page + K * per_candidate + allowance;

    let (small, scanned_small) = peak_of(u64::from(PAGE), &query);
    let (large, scanned_large) = peak_of(20_000, &query);
    assert_eq!(scanned_small, u64::from(PAGE));
    assert_eq!(scanned_large, 20_000);
    assert!(
        large <= bound,
        "a 20000-row scan peaked at {large} bytes, above one page ({one_page}) plus {K} \
         candidates and the allowance ({bound})"
    );
    // And the peak does not grow with the scope: 78 times the rows, the same
    // ceiling.
    assert!(
        large <= small + K * per_candidate + allowance,
        "peak grew with the scope: {small} bytes for one page, {large} for 20000 rows"
    );
}
