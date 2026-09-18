//! Bounded graph differential fuzzing, using the native tests' independent oracle.
//!
//! This separate byte format does not extend `Action`, `FuzzTarget`, or any of the
//! existing Arbitrary enums, so old saved corpora retain their original meaning.
//! All graph expectations and replica behavior live in one shared source set.

#[path = "../../loro/tests/graph_model/driver.rs"]
mod driver;
#[path = "../../loro/tests/graph_model/native.rs"]
mod native;
#[path = "../../loro/tests/graph_model/oracle.rs"]
mod oracle;
#[path = "../../loro/tests/graph_model/regressions.rs"]
mod regressions;

pub use driver::{failure_trace, run_seed};
pub use regressions::pending_snapshot_61516;

/// Maximum randomized steps per libFuzzer input, in addition to fixed bootstrap
/// and final convergence checks. This samples histories, not all possible states.
pub const MAX_STEPS: usize = 120;
/// The adapter reads only this many bytes. Short inputs are zero-padded.
pub const INPUT_LEN: usize = 10;

/// Stable, graph-only input interpretation for corpus replay and minimization.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GraphInput {
    Random {
        seed: u64,
        steps: usize,
    },
    /// The fixed historical-fork/pending/snapshot schedule labelled seed 61516.
    PendingSnapshot61516,
}

impl GraphInput {
    /// Bytes 0..8 are the little-endian seed, byte 8 is the step budget clamped
    /// to MAX_STEPS, and bit 0 of byte 9 selects the saved regression instead.
    /// Missing bytes are zero; trailing bytes have no effect.
    pub fn from_bytes(bytes: &[u8]) -> Self {
        let mut input = [0; INPUT_LEN];
        let len = bytes.len().min(INPUT_LEN);
        input[..len].copy_from_slice(&bytes[..len]);
        if input[9] & 1 != 0 {
            return Self::PendingSnapshot61516;
        }
        Self::Random {
            seed: u64::from_le_bytes(input[..8].try_into().unwrap()),
            steps: usize::from(input[8]).min(MAX_STEPS),
        }
    }

    /// Execute the shared native/model differential runner without swallowing
    /// assertion failures. This also works outside libFuzzer for fast replay.
    pub fn run(self) {
        match self {
            Self::Random { seed, steps } => run_seed(seed, steps.min(MAX_STEPS)),
            Self::PendingSnapshot61516 => pending_snapshot_61516(),
        }
    }
}

/// Run a single bounded libFuzzer input through three real Loro documents.
pub fn fuzz_graph(bytes: &[u8]) {
    GraphInput::from_bytes(bytes).run();
}
