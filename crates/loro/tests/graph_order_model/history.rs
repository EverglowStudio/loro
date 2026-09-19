use super::{oracle::History, support::*};
use loro::{ExportMode, Frontiers, GraphOrderTarget as At, ID};

#[test]
fn snapshots_updates_shallow_state_only_and_json_preserve_editable_registers() {
    let (seed, n, e) = fixture(&["80", "80", "80", "c080"]);
    let g = seed.get_graph("g");
    let boundary = seed.state_frontiers();
    g.reorder_edge(e[3], At::Before(e[1])).unwrap();
    g.delete_edge(e[2]).unwrap();
    seed.commit();
    let expected = history(&seed);
    for mode in 0..5 {
        let source = copy(&seed, 40 + mode);
        let receiver = doc(50 + mode);
        match mode {
            0 => {
                receiver
                    .import(&source.export(ExportMode::Snapshot).unwrap())
                    .unwrap();
            }
            1 => {
                receiver
                    .import(&source.export(ExportMode::all_updates()).unwrap())
                    .unwrap();
            }
            2 => {
                receiver
                    .import(
                        &source
                            .export(ExportMode::shallow_snapshot(&boundary))
                            .unwrap(),
                    )
                    .unwrap();
            }
            3 => {
                receiver
                    .import(&source.export(ExportMode::StateOnly(None)).unwrap())
                    .unwrap();
            }
            4 => import_json(&receiver, json(&source)),
            _ => unreachable!(),
        }
        check(&receiver, "g", &expected);
        let h = receiver.get_graph("g");
        assert_eq!(order(&h, n), vec![e[0], e[3], e[1]]);
        let hidden_position = h.edge_record(e[2]).unwrap().position;
        h.restore_edge(e[2]).unwrap();
        assert_eq!(h.edge_record(e[2]).unwrap().position, hidden_position);
        assert_eq!(order(&h, n), vec![e[0], e[3], e[1], e[2]]);
        h.reorder_edge(e[2], At::Start).unwrap();
        let fresh = h.create_edge_at(n, n, At::After(e[3])).unwrap();
        receiver.commit();
        // Export only the tail: shallow/state-only history need not export pruned ops.
        source
            .import(
                &receiver
                    .export(ExportMode::updates(&seed.oplog_vv()))
                    .unwrap(),
            )
            .unwrap();
        assert_eq!(
            order(&source.get_graph("g"), n),
            vec![e[2], e[0], e[3], fresh, e[1]],
            "mode={mode}"
        );
        assert_eq!(source.get_deep_value(), receiver.get_deep_value());
        check(&receiver, "g", &history(&source));
    }
}

#[test]
fn checkout_retreats_positions_and_can_fork_an_editable_historical_version() {
    let (d, n, e) = fixture(&["4080", "80", "c080"]);
    let g = d.get_graph("g");
    let original = d.state_frontiers();
    let original_history = history(&d);
    g.reorder_edge(e[1], At::End).unwrap();
    d.commit();
    let moved = d.state_frontiers();
    let moved_history = history(&d);
    g.delete_node(n).unwrap();
    d.commit();
    let hidden = d.state_frontiers();
    let hidden_history = history(&d);
    for (at, expected) in [
        (&original, &original_history),
        (&hidden, &hidden_history),
        (&moved, &moved_history),
        (&original, &original_history),
    ] {
        d.checkout(at).unwrap();
        check(&d, "g", expected);
    }
    d.checkout(&Frontiers::default()).unwrap();
    assert!(g.edge_records().is_empty());
    assert_eq!(g.index_of_out_edge(e[1]), None);
    d.checkout_to_latest();
    check(&d, "g", &hidden_history);
    let branch = d.fork_at(&original).unwrap();
    branch.set_peer_id(22).unwrap();
    branch.get_graph("g").reorder_edge(e[2], At::Start).unwrap();
    branch.commit();
    assert_eq!(order(&branch.get_graph("g"), n), vec![e[2], e[0], e[1]]);
    sync(&d, &branch);
    assert!(order(&g, n).is_empty());
    g.restore_node(n).unwrap();
    d.commit();
    sync(&d, &branch);
    assert_eq!(order(&g, n), vec![e[2], e[0], e[1]]);
    check(&d, "g", &history(&branch));
}

#[test]
fn pending_duplicate_packets_activate_only_after_dependencies_arrive() {
    let source = doc(1);
    let g = source.get_graph("g");
    let n = g.create_node().unwrap();
    let a = g.create_edge(n, n).unwrap();
    source.commit();
    let baseline = source.export(ExportMode::snapshot()).unwrap();
    let vv0 = source.oplog_vv();
    let b = g.create_edge(n, n).unwrap();
    source.commit();
    let create = source.export(ExportMode::updates(&vv0)).unwrap();
    let vv1 = source.oplog_vv();
    g.reorder_edge(b, At::Start).unwrap();
    source.commit();
    let moved = source.export(ExportMode::updates(&vv1)).unwrap();
    let expected = history(&source);
    for snapshot_first in [false, true] {
        let receiver = doc(2);
        if snapshot_first {
            receiver.import(&baseline).unwrap();
        }
        let pending = receiver.import(&moved).unwrap();
        assert!(pending.pending.is_some());
        assert!(receiver.import(&moved).unwrap().success.is_empty());
        assert!(receiver.get_graph("g").get_edge(b).is_none());
        if !snapshot_first {
            assert!(receiver.import(&create).unwrap().pending.is_some());
            assert!(receiver.oplog_vv().is_empty());
            assert!(receiver.import(&baseline).unwrap().pending.is_none());
        } else {
            assert!(receiver.import(&create).unwrap().pending.is_none());
        }
        assert_eq!(order(&receiver.get_graph("g"), n), vec![b, a]);
        check(&receiver, "g", &expected);
        let vv = receiver.oplog_vv();
        for bytes in [&moved, &create, &baseline] {
            receiver.import(bytes).unwrap();
        }
        assert_eq!(receiver.oplog_vv(), vv);
        assert_eq!(receiver.get_pending_txn_len(), 0);
    }
}

#[test]
fn legal_partial_collision_batch_has_a_deterministic_queryable_order() {
    let (source, n, e) = fixture(&["80", "80", "80", "c080"]);
    let base = source.oplog_vv();
    let receiver = copy(&source, 2);
    source
        .get_graph("g")
        .reorder_edge(e[3], At::Before(e[1]))
        .unwrap();
    source.commit();
    let peer = source.peer_id();
    let start = base.get(&peer).copied().unwrap_or(0);
    let end = *source.oplog_vv().get(&peer).unwrap();
    assert!(end - start >= 3, "fixture must include auxiliary writes");
    let mut previous = base.clone();
    for counter in start + 1..=end {
        let mut cut = base.clone();
        cut.set_end(ID::new(peer, counter));
        let prefix = json_between(&source, &previous, &cut);
        import_json(&receiver, prefix);
        let expected = History::from_json(&json_between(&source, &Default::default(), &cut));
        check(&receiver, "g", &expected);
        assert_eq!(
            receiver.get_pending_txn_len(),
            0,
            "import repaired a partial batch"
        );
        assert_eq!(receiver.oplog_vv(), cut);
        previous = cut;
    }
    assert_eq!(
        order(&receiver.get_graph("g"), n),
        vec![e[0], e[3], e[1], e[2]]
    );
    receiver
        .get_graph("g")
        .reorder_edge(e[2], At::Start)
        .unwrap();
    assert_eq!(
        order(&receiver.get_graph("g"), n),
        vec![e[2], e[0], e[3], e[1]]
    );
}
