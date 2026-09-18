use fuzz::graph::{fuzz_graph, run_seed, GraphInput, INPUT_LEN, MAX_STEPS};

#[test]
fn graph_byte_contract_is_stable_and_bounded() {
    assert_eq!(
        GraphInput::from_bytes(&[]),
        GraphInput::Random { seed: 0, steps: 0 }
    );
    assert_eq!(
        GraphInput::from_bytes(&[0x4c, 0xf0]),
        GraphInput::Random {
            seed: 61516,
            steps: 0
        }
    );
    let mut input = [0xff; INPUT_LEN];
    input[9] = 0;
    assert_eq!(
        GraphInput::from_bytes(&input),
        GraphInput::Random {
            seed: u64::MAX,
            steps: MAX_STEPS
        }
    );
    let mut longer = input.to_vec();
    longer.extend([0x4c, 0xf0, 1]);
    assert_eq!(
        GraphInput::from_bytes(&longer),
        GraphInput::from_bytes(&input)
    );
    for budget in 0..=255 {
        input[8] = budget;
        assert_eq!(
            GraphInput::from_bytes(&input),
            GraphInput::Random {
                seed: u64::MAX,
                steps: usize::from(budget).min(MAX_STEPS)
            }
        );
    }
    input[9] = 1;
    assert_eq!(
        GraphInput::from_bytes(&input),
        GraphInput::PendingSnapshot61516
    );
}

#[test]
fn graph_saved_pending_snapshot_61516() {
    let input = include_bytes!("graph_corpus/pending-snapshot-61516");
    assert_eq!(input.len(), INPUT_LEN);
    assert_eq!(u64::from_le_bytes(input[..8].try_into().unwrap()), 61516);
    assert_eq!(
        GraphInput::from_bytes(input),
        GraphInput::PendingSnapshot61516
    );
    fuzz_graph(input);
}

#[test]
fn graph_fixed_random_corpus_uses_native_model() {
    for (input, expected) in [
        (
            include_bytes!("graph_corpus/random-1"),
            GraphInput::Random { seed: 1, steps: 32 },
        ),
        (
            include_bytes!("graph_corpus/random-42"),
            GraphInput::Random {
                seed: 42,
                steps: 120,
            },
        ),
    ] {
        assert_eq!(GraphInput::from_bytes(input), expected);
        fuzz_graph(input);
    }
    // An empty byte slice still exercises the shared topology, reverse pending
    // imports and final snapshots; it is not an early-return fuzzing no-op.
    fuzz_graph(&[]);
}

#[test]
fn graph_seed_runner_is_available_without_libfuzzer() {
    run_seed(7, 8);
}
