#![no_main]

use fuzz::graph::{failure_trace, fuzz_graph, GraphInput};
use libfuzzer_sys::fuzz_target;
use std::cell::Cell;
use std::sync::Once;

thread_local! {
    static CURRENT_INPUT: Cell<Option<GraphInput>> = const { Cell::new(None) };
}

fn install_trace_hook() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        // libfuzzer-sys aborts in its panic hook before the driver's catch_unwind
        // can print its trace. Preserve that hook and report the replay input
        // and shared bounded action trace before calling it.
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            CURRENT_INPUT.with(|current| match current.get() {
                Some(GraphInput::Random { seed, steps }) => eprintln!(
                    "graph input seed={seed} steps={steps}\nreplay from Loro root: LORO_GRAPH_SEED={seed} LORO_GRAPH_STEPS={steps} CARGO_INCREMENTAL=0 cargo test -p loro --test graph_model native_random -j 2 -- --nocapture"
                ),
                Some(GraphInput::PendingSnapshot61516) => eprintln!(
                    "graph saved regression seed=61516\nreplay from Loro root: CARGO_INCREMENTAL=0 cargo test -p loro --test graph_model native_historical_fork_pending_chunks_and_snapshot_merge -j 2 -- --nocapture"
                ),
                None => {}
            });
            eprintln!("last graph actions:\n{}", failure_trace());
            previous(info);
        }));
    });
}

fuzz_target!(|bytes: &[u8]| {
    // The pinned libfuzzer-sys 0.4.7 macro has no initialization clause.
    // Its own initialization has run before this first input is delivered.
    install_trace_hook();
    CURRENT_INPUT.with(|current| current.set(Some(GraphInput::from_bytes(bytes))));
    fuzz_graph(bytes);
    CURRENT_INPUT.with(|current| current.set(None));
});
