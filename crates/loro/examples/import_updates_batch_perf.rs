//! Compare batch APIs and sequential import in one optimized native binary.
//! The original six cases are unchanged; large-text cases add the third strategy.
//! No allocation claims.
//!
//! CARGO_INCREMENTAL=0 cargo build -p loro --release --example import_updates_batch_perf -j 2
//! ./target/release/examples/import_updates_batch_perf
//!
//! JSON Lines on stdout: one configuration record, then each timed invocation.
//! Fixtures and slice views are built before timing. Each interval includes only
//! target creation, import, and identical full correctness checks; target teardown
//! and JSON serialization are excluded. Warmups are emitted but marked separately.

use std::time::{Duration, Instant};

use loro::{ExportMode, ImportStatus, LoroDoc, LoroValue, VersionRange, VersionVector};
use serde_json::json;

const WARMUP: usize = 2;
const SAMPLES: usize = 10;
const SOURCE_PEER: u64 = 42;
const TARGET_PEER: u64 = 43;
const BODY: &str = "fixed update body: 0123456789 abcdefghijklmnopqrstuvwxyz\n";

struct Fixture {
    updates: Vec<Vec<u8>>,
    vv: VersionVector,
    value: LoroValue,
    status: ImportStatus,
}

fn fixture(count: usize) -> Fixture {
    let source = LoroDoc::new();
    source.set_peer_id(SOURCE_PEER).unwrap();
    source.set_record_timestamp(false);
    let text = source.get_text("body");
    let mut updates = Vec::with_capacity(count);
    let mut vv = VersionVector::default();
    for _ in 0..count {
        text.insert(text.len_unicode(), BODY).unwrap();
        updates.push(source.export(ExportMode::updates(&vv)).unwrap());
        vv = source.oplog_vv();
    }
    assert_eq!(text.to_string(), BODY.repeat(count));
    Fixture {
        updates,
        status: ImportStatus {
            success: VersionRange::from_vv(&vv),
            pending: None,
        },
        vv,
        value: source.get_deep_value(),
    }
}

#[derive(Clone, Copy)]
enum Strategy {
    Batch,
    UpdatesBatch,
    Sequential,
}

impl Strategy {
    fn name(self) -> &'static str {
        match self {
            Self::Batch => "import_batch",
            Self::UpdatesBatch => "import_updates_batch",
            Self::Sequential => "sequential_import",
        }
    }
}

fn measure(fixture: &Fixture, borrowed: &[&[u8]], strategy: Strategy) -> Duration {
    let start = Instant::now();
    let target = LoroDoc::new();
    target.set_peer_id(TARGET_PEER).unwrap();
    let status = match strategy {
        Strategy::UpdatesBatch => target.import_updates_batch(borrowed).unwrap(),
        Strategy::Batch => target.import_batch(&fixture.updates).unwrap(),
        Strategy::Sequential => {
            let mut total = ImportStatus::default();
            for update in borrowed {
                let status = target.import(update).unwrap();
                for (&peer, &(start, end)) in status.success.iter() {
                    let (start, end) = total
                        .success
                        .get(&peer)
                        .map_or((start, end), |&(a, b)| (a.min(start), b.max(end)));
                    total.success.insert(peer, start, end);
                }
                // Fixtures are causally closed. The last successful import must
                // have unlocked their remaining operations, even in reverse order.
                total.pending = status.pending;
            }
            total
        }
    };
    // Keep the same full assertions inside ALL timed paths, including pending.
    assert_eq!(target.oplog_vv(), fixture.vv);
    assert_eq!(target.get_deep_value(), fixture.value);
    assert_eq!(status, fixture.status);
    assert!(!target.is_detached());
    assert_eq!(target.state_frontiers(), target.oplog_frontiers());
    let elapsed = start.elapsed();
    drop(target);
    elapsed
}

fn main() {
    println!(
        "{}",
        json!({
            "kind": "configuration",
            "warmup_per_api": WARMUP,
            "samples_per_api": SAMPLES,
            "increments": [1, 32, 128],
            "orders": ["ordered", "reversed"],
            "source_peer": SOURCE_PEER,
            "target_peer": TARGET_PEER,
            "body": BODY,
            "timing": "empty target creation + import + full VV/body/status/attachment checks",
            "excluded": "fixture generation, borrowed views, target teardown, JSON output",
            "allocation_measurement": false,
            "large_text_case": {
                "seed_sizes_bytes": [24 * 1024, 48 * 1024, 64 * 1024],
                "tail_replacements": 12,
                "strategies": ["import_batch", "import_updates_batch", "sequential_import"],
                "orders": ["ordered", "reversed"],
            },
            "debug_assertions": cfg!(debug_assertions),
            "architecture": std::env::consts::ARCH,
            "os": std::env::consts::OS,
        })
    );
    for count in [1, 32, 128] {
        let mut fixture = fixture(count);
        for reversed in [false, true] {
            if reversed {
                fixture.updates.reverse();
            }
            // Both APIs consume the same Vec-backed buffers in the same order.
            // No per-sample clones or adapter allocations are included in timing.
            let borrowed: Vec<_> = fixture.updates.iter().map(Vec::as_slice).collect();
            let input_bytes: usize = borrowed.iter().map(|blob| blob.len()).sum();
            for (phase, iterations) in [("warmup", WARMUP), ("sample", SAMPLES)] {
                for sample in 0..iterations {
                    // Alternate which API runs first to reduce systematic order bias.
                    let order = if sample % 2 == 0 {
                        [false, true]
                    } else {
                        [true, false]
                    };
                    let mut results = Vec::with_capacity(2);
                    for (position, updates_only) in order.into_iter().enumerate() {
                        let strategy = if updates_only {
                            Strategy::UpdatesBatch
                        } else {
                            Strategy::Batch
                        };
                        let elapsed = measure(&fixture, &borrowed, strategy);
                        results.push(json!({
                            "kind": "measurement",
                            "phase": phase,
                            "sample": sample,
                            "position_in_pair": position,
                            "api": if updates_only { "import_updates_batch" } else { "import_batch" },
                            "increments": count,
                            "order": if reversed { "reversed" } else { "ordered" },
                            "input_bytes": input_bytes,
                            "elapsed_us": elapsed.as_secs_f64() * 1_000_000.0,
                            "elapsed_ms": elapsed.as_secs_f64() * 1_000.0,
                            "validated": true,
                            "pending": null,
                        }));
                    }
                    // Print after both timed calls, outside their measurement windows.
                    for result in results {
                        println!("{result}");
                    }
                }
            }
        }
    }
    run_large_text_cases();
}

fn large_text_fixture() -> Fixture {
    let source = LoroDoc::new();
    source.set_peer_id(SOURCE_PEER).unwrap();
    source.set_record_timestamp(false);
    let text = source.get_text("body");
    let mut vv = VersionVector::default();
    let mut updates = Vec::new();
    for size in [24 * 1024, 48 * 1024, 64 * 1024] {
        text.insert(text.len_unicode(), &"s".repeat(size - text.len_unicode()))
            .unwrap();
        updates.push(source.export(ExportMode::updates(&vv)).unwrap());
        vv = source.oplog_vv();
    }
    for i in 0..12 {
        text.delete(text.len_unicode() - 1, 1).unwrap();
        text.insert(text.len_unicode(), if i % 2 == 0 { "a" } else { "b" })
            .unwrap();
        updates.push(source.export(ExportMode::updates(&vv)).unwrap());
        vv = source.oplog_vv();
    }
    assert_eq!(updates.len(), 15);
    assert_eq!(text.to_string(), format!("{}b", "s".repeat(64 * 1024 - 1)));
    Fixture {
        updates,
        status: ImportStatus {
            success: VersionRange::from_vv(&vv),
            pending: None,
        },
        vv,
        value: source.get_deep_value(),
    }
}

fn run_large_text_cases() {
    let mut fixture = large_text_fixture();
    let strategies = [
        Strategy::Batch,
        Strategy::UpdatesBatch,
        Strategy::Sequential,
    ];
    for reversed in [false, true] {
        if reversed {
            fixture.updates.reverse();
        }
        let borrowed: Vec<_> = fixture.updates.iter().map(Vec::as_slice).collect();
        let input_bytes: usize = borrowed.iter().map(|blob| blob.len()).sum();
        for (phase, iterations) in [("warmup", WARMUP), ("sample", SAMPLES)] {
            for sample in 0..iterations {
                // Rotate the first strategy, without changing the original six
                // cases' alternating old/new schedule or their fixtures.
                let results: Vec<_> = (0..strategies.len())
                    .map(|position| {
                        let strategy = strategies[(sample + position) % strategies.len()];
                        (position, strategy, measure(&fixture, &borrowed, strategy))
                    })
                    .collect();
                for (position, strategy, elapsed) in results {
                    println!(
                        "{}",
                        json!({
                            "kind": "measurement",
                            "scenario": "large_text_tail_replace",
                            "phase": phase,
                            "sample": sample,
                            "position_in_round": position,
                            "api": strategy.name(),
                            "increments": fixture.updates.len(),
                            "seed_sizes_bytes": [24 * 1024, 48 * 1024, 64 * 1024],
                            "tail_replacements": 12,
                            "order": if reversed { "reversed" } else { "ordered" },
                            "input_bytes": input_bytes,
                            "elapsed_us": elapsed.as_secs_f64() * 1_000_000.0,
                            "elapsed_ms": elapsed.as_secs_f64() * 1_000.0,
                            "validated": true,
                            "pending": null,
                        })
                    );
                }
            }
        }
    }
}
