use super::{
    driver::{run_seed, with_run},
    native::graph,
    oracle::{Action, Graph, Object},
    regressions::pending_snapshot_61516,
};
use loro_common::GraphNodeId;

const G: Graph = Graph(0);

#[test]
fn native_random_three_replica_differential() {
    let seeds = match std::env::var("LORO_GRAPH_SEED") {
        Ok(value) => vec![value
            .parse::<u64>()
            .expect("LORO_GRAPH_SEED must be a decimal u64")],
        Err(std::env::VarError::NotPresent) => vec![1, 7, 42, 0xCA05_A1, 0xDE1E_7E],
        Err(error) => panic!("invalid LORO_GRAPH_SEED: {error}"),
    };
    let steps = match std::env::var("LORO_GRAPH_STEPS") {
        Ok(value) => value
            .parse::<usize>()
            .expect("LORO_GRAPH_STEPS must be a nonnegative integer"),
        Err(std::env::VarError::NotPresent) => 120,
        Err(error) => panic!("invalid LORO_GRAPH_STEPS: {error}"),
    };
    for seed in seeds {
        run_seed(seed, steps);
    }
}

#[test]
fn native_partial_restore_keeps_the_unobserved_delete() {
    for on_edge in [false, true] {
        with_run(
            "native_partial_restore_keeps_the_unobserved_delete",
            0xDE1E_7E + u64::from(on_edge),
            |run| {
                let x = run.node(0, G);
                let y = run.node(0, G);
                let edge = run.edge(0, y, x);
                let object = if on_edge {
                    Object::Edge(edge)
                } else {
                    Object::Node(x)
                };
                run.send_all(0, 1);
                run.send_all(0, 2);
                let before_delete = run.save_cut(0);
                let delete_a = run.act(0, Action::Delete(object));
                let delete_b = run.act(1, Action::Delete(object));
                let partial_restore = run.act(0, Action::Restore(object));
                let partial_cut = run.save_cut(0);
                assert!(run.state(0, G).nodes[&x].alive);
                // Restore arrives before its own dependency. The other Delete may be
                // applied in the meantime; packet IDs are duplicated deliberately.
                run.deliver(
                    2,
                    &[partial_restore, delete_b, partial_restore, delete_a],
                    false,
                );
                run.deliver(0, &[delete_b], false);
                run.deliver(1, &[partial_restore, delete_a, delete_a], true);
                for replica in 0..3 {
                    let state = run.state(replica, G);
                    assert_eq!(state.nodes[&x].alive, on_edge);
                    assert_eq!(state.edges[&edge].alive, !on_edge);
                    assert!(!state.edges[&edge].visible);
                }
                run.checkout_roundtrip(0, partial_cut);
                run.act(0, Action::Restore(object));
                run.converge();
                assert!(run.state(2, G).edges[&edge].visible);
                run.checkout_roundtrip(2, before_delete);
            },
        );
    }
}

#[test]
fn native_endpoint_restore_events_preserve_parallel_edge_identity() {
    with_run(
        "native_endpoint_restore_events_preserve_parallel_edge_identity",
        0xED6E,
        |run| {
            let x = run.node(0, G);
            let y = run.node(0, G);
            let isolated = run.node(0, G);
            let old = run.edge(0, x, y);
            let deleted_parallel = run.edge(0, x, y);
            let reverse = run.edge(0, y, x);
            let self_loop = run.edge(0, x, x);
            let foreign = run.node(0, Graph(1));
            run.act(
                0,
                Action::Set {
                    object: Object::Node(x),
                    key: "name".into(),
                    value: "same name".into(),
                },
            );
            run.send_all(0, 1);
            run.send_all(0, 2);
            let initial_cut = run.save_cut(0);
            run.act(0, Action::Delete(Object::Edge(deleted_parallel)));
            run.act(0, Action::Delete(Object::Node(x)));
            // Concurrent creation of another x->y edge must not inherit the
            // deletion of the existing parallel EdgeId.
            let unseen = run.edge(1, x, y);
            let replacement = run.node(1, G);
            run.act(
                1,
                Action::Set {
                    object: Object::Node(replacement),
                    key: "name".into(),
                    value: "same name".into(),
                },
            );
            run.send_all(1, 0);
            let hidden = run.state(0, G);
            assert!(hidden.visible_edges().is_empty());
            assert!(hidden.nodes[&y].alive && hidden.nodes[&isolated].alive);
            assert!(hidden.edges[&unseen].alive && hidden.edges[&old].alive);
            assert_eq!(hidden.edges[&old].source, x);
            assert!(hidden.outgoing(replacement).is_empty());
            // The event mirror cannot derive these edge changes from the node:
            // the public GraphDiff must explicitly carry disappearance/reappearance.
            run.act(0, Action::Restore(Object::Node(x)));
            assert_eq!(
                run.state(0, G).visible_edges(),
                [old, reverse, self_loop, unseen].into()
            );
            assert!(!run.state(0, G).edges[&deleted_parallel].alive);

            run.note("cross-graph and missing endpoint rejection must not write operations");
            let before = run.doc(0).oplog_vv();
            let frontiers = run.doc(0).state_frontiers();
            let main = graph(run.doc(0), G);
            let other = graph(run.doc(0), Graph(1));
            assert!(main
                .create_edge(run.ids.nodes[&x], run.ids.nodes[&foreign])
                .is_err());
            assert!(other.delete_node(run.ids.nodes[&x]).is_err());
            assert!(other.restore_edge(run.ids.edges[&old]).is_err());
            assert!(other.node_meta(run.ids.nodes[&x]).is_err());
            assert!(other.edge_meta(run.ids.edges[&old]).is_err());
            assert!(main
                .create_edge(run.ids.nodes[&x], GraphNodeId::new(u64::MAX, 42))
                .is_err());
            assert_eq!(run.doc(0).get_pending_txn_len(), 0);
            assert_eq!(run.doc(0).oplog_vv(), before);
            assert_eq!(run.doc(0).state_frontiers(), frontiers);
            run.check(0);
            run.converge();
            run.checkout_roundtrip(1, initial_cut);
        },
    );
}

#[test]
fn native_deleted_owners_retain_concurrent_nested_text() {
    with_run(
        "native_deleted_owners_retain_concurrent_nested_text",
        0x7E87,
        |run| {
            let x = run.node(0, G);
            let y = run.node(0, G);
            let edge = run.edge(0, x, y);
            let objects = [Object::Node(x), Object::Edge(edge)];
            // The shared Text identity is established once. Opposite-end concurrent
            // insertions have an exact, independent expected string, with Unicode.
            for object in objects {
                run.act(
                    0,
                    Action::SeedText {
                        object,
                        key: "body".into(),
                        value: "中心🦀".into(),
                    },
                );
            }
            run.send_all(0, 1);
            run.send_all(0, 2);
            let text_baseline = run.save_cut(0);
            for object in objects {
                run.act(0, Action::Delete(object));
                run.act(
                    1,
                    Action::PrefixText {
                        object,
                        key: "body".into(),
                        value: "左/".into(),
                    },
                );
                run.act(
                    2,
                    Action::SuffixText {
                        object,
                        key: "body".into(),
                        value: "/右".into(),
                    },
                );
                run.act(
                    1,
                    Action::Set {
                        object,
                        key: "writer/2".into(),
                        value: "left".into(),
                    },
                );
                run.act(
                    2,
                    Action::Set {
                        object,
                        key: "right".into(),
                        value: "right".into(),
                    },
                );
            }
            run.send_all(1, 0);
            run.send_all(2, 0);
            let hidden = run.state(0, G);
            assert!(!hidden.nodes[&x].alive && !hidden.edges[&edge].alive);
            for object in objects {
                assert_eq!(hidden.properties(object).texts["body"], "左/中心🦀/右");
                assert_eq!(hidden.properties(object).scalars.len(), 2);
            }
            // A property write after observing deletion is also not a lifecycle op.
            run.act(
                0,
                Action::Set {
                    object: Object::Edge(edge),
                    key: "while-deleted".into(),
                    value: "kept".into(),
                },
            );
            run.act(0, Action::Restore(Object::Node(x)));
            assert!(!run.state(0, G).edges[&edge].visible);
            run.act(0, Action::Restore(Object::Edge(edge)));
            run.converge();
            run.checkout_roundtrip(2, text_baseline);
        },
    );
}

#[test]
fn native_historical_fork_pending_chunks_and_snapshot_merge() {
    pending_snapshot_61516();
}
