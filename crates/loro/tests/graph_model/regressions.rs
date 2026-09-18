//! Saved schedules run by both native regression tests and the graph fuzz target.

use super::{
    driver::with_run,
    oracle::{Action, Graph, Object},
};

/// Seed 61516 identified a directed schedule, not the random generator's output.
/// Keep the actual actions here so replay does not mistake `run_seed(61516, n)`
/// for the missing-dependency/snapshot counterexample.
pub fn pending_snapshot_61516() {
    with_run(
        "native_historical_fork_pending_chunks_and_snapshot_merge",
        61516,
        |run| {
            let graph = Graph(0);
            let x = run.node(0, graph);
            let y = run.node(0, graph);
            let nodes_cut = run.save_cut(0);
            let old = run.edge(0, x, y);
            let original_cut = run.save_cut(0);
            let deleted = run.act(0, Action::Delete(Object::Node(x)));
            // Only the descendant packet is delivered. The receiver must retain it
            // as pending, without manufacturing either endpoint or the edge.
            run.deliver(2, &[deleted, old.birth, deleted], false);
            assert!(run.version(2).is_empty());
            run.fork_editor(0, 1, nodes_cut);
            let branch = run.edge(1, y, x);
            run.act(
                1,
                Action::Set {
                    object: Object::Node(x),
                    key: "branch".into(),
                    value: "historical edit".into(),
                },
            );
            // A snapshot supplies missing dependencies and merges with already
            // received updates. It must not overwrite the independently deleted x.
            run.merge_snapshot(1, 2);
            let merged = run.state(2, graph);
            assert!(!merged.nodes[&x].alive);
            assert!(merged.edges[&old].alive && merged.edges[&branch].alive);
            assert!(merged.visible_edges().is_empty());
            run.deliver(1, &[deleted, old.birth, old.birth], true);
            run.act(1, Action::Restore(Object::Node(x)));
            run.converge();
            assert_eq!(run.state(0, graph).visible_edges(), [old, branch].into());
            run.checkout_roundtrip(0, nodes_cut);
            run.checkout_roundtrip(0, original_cut);
        },
    );
}
