# Graph differential fuzz target

This target uses the **same** slow causal oracle, public API adapter, three-replica
driver, and saved pending/snapshot schedule as `loro --test graph_model`.
`../graph.rs` references those source modules with `#[path]`; it does not copy
their implementation or reuse production GraphOp/GraphState/index algorithms.
The shared `driver::run_seed(seed, steps)` is also callable without libFuzzer.
Graph IDs come from the public `loro` exports, so no dependency is added.

The existing `Action`, `FuzzTarget`, and Arbitrary enum layouts are untouched by
this target. Existing saved corpus decoding does not acquire a Graph variant.

## Input and scope

The graph-only byte format reads at most ten bytes:

| Bytes | Meaning |
| --- | --- |
| 0–7 | Little-endian u64 random seed |
| 8 | Randomized step count, clamped to 120 |
| 9, bit 0 | 0: random schedule; 1: saved pending/snapshot regression |

Missing bytes are zero-padded; trailing bytes are ignored. Empty input still runs
the shared bootstrap, reversed dependency delivery, convergence, and snapshot
checks. A generated step can include multiple deliveries or a fork plus an edit;
120 is a bound on driver steps, not an exact number of CRDT operations.

Randomized schedules include offline edits, partial/duplicate/reordered delivery,
batched complete update blobs, ordinary delete/restore, scalar metadata writes,
historical checkout and diff, fork editing, snapshot merging and reconstruction.
They compare records, typed identity, endpoints, alive/visible state, properties,
adjacency, neighbors, counts and event reconstruction against the independent
oracle. Scalar keys have one writer. General Text operations, Undo/Redo, malformed
wire input, shallow history, and helper repair policies are outside this random
surface; the native directed suites retain their own contracts. This is bounded
sampling, not exhaustive state-space exploration or a correctness proof.

`tests/graph_corpus/pending-snapshot-61516` is a saved **directed schedule**, not
the output of the random generator at seed 61516. It selects the single shared
`pending_snapshot_61516()` function. That reproduces an initially empty receiver
holding peer 1 operations `[2,4)` pending, followed by a historical fork snapshot
supplying `[0,2)`: the expected applied VV is `{1:4, 100:2}`. The original failure
and all subsequent assertions are retained. `random-1` and `random-42` exercise
the random adapter with 32 and 120 steps.

libfuzzer-sys aborts from its panic hook before normal unwind-based diagnostics.
The target wraps and preserves that hook to print the decoded seed/steps or saved
regression name and the shared driver's last 32 actions first. The bounded trace
is thread-local, so parallel native tests cannot mix their histories. LibFuzzer
saves the crashing bytes; native replay uses the same trace and assertions. No
failures are swallowed. Reducing byte 8 shortens a random prefix without changing
its seed; use `LORO_GRAPH_STEPS` to replay and find the smallest failing prefix,
then save a directed case. A seed mutation changes the whole schedule and is not
an action-level reducer.

## Running and replay

Run from the Loro repository root. Native tests use the repository toolchain.
The direct libFuzzer commands need nightly and a C++17 compiler (the existing
macOS command-line tools suffice); neither `cargo-fuzz` nor `nextest` is required.
The commands limit builds to two jobs and disable incremental compilation.

```sh
# Existing model suite and new fuzz-crate regression/byte-mapping tests.
CARGO_INCREMENTAL=0 cargo test -p loro --test graph_model -j 2
CARGO_INCREMENTAL=0 cargo test -p fuzz --test graph -j 2

# Named regression, independent of random schedule generation.
CARGO_INCREMENTAL=0 cargo test -p fuzz --test graph graph_saved_pending_snapshot_61516 -j 2 -- --exact --nocapture

# Native replay of a random fuzz failure; substitute its printed seed and steps.
LORO_GRAPH_SEED=42 LORO_GRAPH_STEPS=120 CARGO_INCREMENTAL=0 cargo test -p loro --test graph_model native_random -j 2 -- --nocapture
```

The existing `libfuzzer-sys` default `link_libfuzzer` feature builds the bundled
C++ sources, including `FuzzerMain.cpp`, which provides the executable's `main`.
The `#![no_main]` graph target supplies the callbacks through `fuzz_target!`.
Cargo can therefore build and run this bin directly. Its manual coverage flags
are passed through `RUSTFLAGS` so the driver and dependencies are instrumented,
with a coverage PC table and release assertions enabled. An explicit host target
keeps those flags out of build scripts and proc macros.

```sh
# Short smoke without cargo-fuzz. First corpus is mutable; tracked seeds are input-only.
LORO_GRAPH_FUZZ_TARGET="$(rustc +nightly -vV | sed -n 's/^host: //p')"
LORO_GRAPH_FUZZ_FLAGS='--cfg fuzzing -Cdebug-assertions=yes -Coverflow-checks=yes -Cpasses=sancov-module -Cllvm-args=-sanitizer-coverage-level=3 -Cllvm-args=-sanitizer-coverage-inline-8bit-counters -Cllvm-args=-sanitizer-coverage-pc-table'
mkdir -p crates/fuzz/fuzz/corpus/graph crates/fuzz/fuzz/artifacts/graph
RUSTFLAGS="$LORO_GRAPH_FUZZ_FLAGS" CARGO_INCREMENTAL=0 \
  cargo +nightly run --manifest-path crates/fuzz/fuzz/Cargo.toml \
  --release --bin graph --target "$LORO_GRAPH_FUZZ_TARGET" --locked -j 2 -- \
  crates/fuzz/fuzz/corpus/graph crates/fuzz/tests/graph_corpus \
  -seed=1 -runs=64 -max_total_time=10 -max_len=10 -timeout=10 \
  -artifact_prefix=crates/fuzz/fuzz/artifacts/graph/

# Exact saved regression through the same executable, reusing the flags above.
RUSTFLAGS="$LORO_GRAPH_FUZZ_FLAGS" CARGO_INCREMENTAL=0 \
  cargo +nightly run --manifest-path crates/fuzz/fuzz/Cargo.toml \
  --release --bin graph --target "$LORO_GRAPH_FUZZ_TARGET" --locked -j 2 -- \
  crates/fuzz/tests/graph_corpus/pending-snapshot-61516 -runs=1
```

For a build-only check, reuse the target and flags defined above:

```sh
RUSTFLAGS="$LORO_GRAPH_FUZZ_FLAGS" CARGO_INCREMENTAL=0 \
  cargo +nightly build --manifest-path crates/fuzz/fuzz/Cargo.toml \
  --release --bin graph --target "$LORO_GRAPH_FUZZ_TARGET" --locked -j 2
```

These commands enable coverage instrumentation but no memory sanitizer, matching
the repository wrapper's macOS arm64 policy (auto disables ASan because of runtime
initialization problems). A successful smoke is a bounded differential run, not
an ASan result.

If `cargo-fuzz` is already installed, the existing wrapper is another entry point:

```sh
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 node scripts/cargo-fuzz-run.mjs graph fuzz/corpus/graph tests/graph_corpus -- -seed=1 -runs=64 -max_total_time=10 -max_len=10 -timeout=10
```

That wrapper invokes `cargo +nightly fuzz run` from `crates/fuzz` and chooses its
platform sanitizer policy. It requires `cargo-fuzz`; the direct commands do not.
`crates/fuzz/fuzz` is a separate Cargo workspace with its own Cargo.lock. Its
lockfile now matches the existing local 1.16.0 packages and in-tree btree;
registry and historical Git dependency versions are unchanged. The smoke
limits execution to 64 runs or ten seconds, excluding compilation; it does not
represent a long-running fuzz campaign.
