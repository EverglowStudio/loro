//! A decision seed per step makes subsequence replay independent of RNG draws
//! in removed steps. Each surviving step still chooses valid objects from the
//! independent model, so reduction cannot manufacture invalid causal fixtures.
use super::{
    driver::Run,
    oracle::{Fact, Id},
    replay,
};
use loro::LoroValue;
use rand::{rngs::StdRng, RngCore, SeedableRng};
use std::{
    collections::BTreeMap,
    panic::{catch_unwind, AssertUnwindSafe},
};

#[derive(Debug, PartialEq, Eq)]
struct Report {
    facts: BTreeMap<Id, Fact>,
    value: LoroValue,
    trace: Vec<String>,
}
struct Failure {
    message: String,
    trace: Vec<String>,
}

fn execute(tape: &[u64]) -> Result<Report, Failure> {
    let mut run = None;
    let result = catch_unwind(AssertUnwindSafe(|| {
        run = Some(Run::new());
        let run = run.as_mut().unwrap();
        for (step, seed) in tape.iter().enumerate() {
            run.random_step(&mut StdRng::seed_from_u64(*seed), step);
        }
        run.converge();
    }));
    match result {
        Ok(()) => {
            let run = run.unwrap();
            Ok(Report {
                facts: run.expected(0).0,
                value: run.docs[0].get_deep_value(),
                trace: run.trace,
            })
        }
        Err(error) => Err(Failure {
            message: error
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| error.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_else(|| "non-string panic".into()),
            trace: run.map_or_else(Vec::new, |run| run.trace),
        }),
    }
}

fn verify_or_shrink(tape: &[u64], label: &str) {
    let Err(original) = execute(tape) else {
        return;
    };
    // Require the entire assertion payload to match. A different assertion or
    // a different mismatch is not evidence that the original failure shrank.
    let minimal = replay::minimize(tape, |candidate| {
        execute(candidate).is_err_and(|failure| failure.message == original.message)
    });
    let failed = execute(&minimal)
        .err()
        .expect("minimal replay must still fail");
    assert_eq!(failed.message, original.message);
    panic!("{label}: {}\nReduced {} decisions to {}.\nReplay: LORO_GRAPH_ORDER_REPLAY='{}' CARGO_INCREMENTAL=0 cargo test -p loro --test graph_order -j 2 replay_driver::shrinkable_three_replica_decision_tapes -- --exact --nocapture\n{}",
        original.message, tape.len(), minimal.len(), replay::encode(&minimal), failed.trace.join("\n"));
}

#[test]
fn shrinkable_three_replica_decision_tapes() {
    if let Ok(encoded) = std::env::var("LORO_GRAPH_ORDER_REPLAY") {
        verify_or_shrink(&replay::decode(&encoded), "explicit decision replay");
        return;
    }
    let seeds = std::env::var("LORO_GRAPH_ORDER_SEED")
        .ok()
        .map(|s| vec![s.parse::<u64>().unwrap()])
        .unwrap_or_else(|| vec![0, 42, 61516, 0xC0111510]);
    let steps = std::env::var("LORO_GRAPH_ORDER_STEPS")
        .ok()
        .map(|s| s.parse().unwrap())
        .unwrap_or(120);
    for seed in seeds {
        let mut rng = StdRng::seed_from_u64(seed);
        let tape: Vec<_> = (0..steps).map(|_| rng.next_u64()).collect();
        verify_or_shrink(&tape, &format!("master seed {seed}"));
    }
}

#[test]
fn decision_tape_replays_the_same_facts_metadata_and_trace() {
    let tape = vec![0, u64::MAX, 42, 61516, (1 << 54) + 4, 9, 8, 7, 6, 5];
    let first = execute(&tape).unwrap_or_else(|f| panic!("{}", f.message));
    let encoded = replay::encode(&tape);
    let second = execute(&replay::decode(&encoded)).unwrap_or_else(|f| panic!("{}", f.message));
    assert_eq!(first, second);
}
