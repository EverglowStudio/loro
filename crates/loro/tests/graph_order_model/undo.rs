use super::support::*;
use loro::{GraphOrderTarget as At, UndoManager};

#[test]
fn two_undos_follow_local_effects_through_compensation_writes_then_redo_twice() {
    let (d, n, e) = fixture(&["2080", "4080", "8080", "c080"]);
    let g = d.get_graph("g");
    let p0 = g.edge_record(e[1]).unwrap().position;
    let mut undo = UndoManager::new(&d);
    undo.set_merge_interval(0);
    assert!(g.reorder_edge(e[1], At::End).unwrap().changed);
    d.commit();
    let p1 = g.edge_record(e[1]).unwrap().position;
    assert!(g.reorder_edge(e[1], At::Start).unwrap().changed);
    d.commit();
    let p2 = g.edge_record(e[1]).unwrap().position;
    assert_ne!(p0, p1);
    assert_ne!(p1, p2);
    assert_ne!(p0, p2);
    undo.undo().unwrap();
    assert_eq!(g.edge_record(e[1]).unwrap().position, p1);
    assert_eq!(order(&g, n), vec![e[0], e[2], e[3], e[1]]);
    let compensation = g.edge_record(e[1]).unwrap().last_order;
    undo.undo().unwrap();
    assert_eq!(
        g.edge_record(e[1]).unwrap().position,
        p0,
        "U2 is compensation for A2, not an independent winner that blocks undoing A1"
    );
    assert_ne!(g.edge_record(e[1]).unwrap().last_order, compensation);
    assert_eq!(order(&g, n), e);
    undo.redo().unwrap();
    assert_eq!(g.edge_record(e[1]).unwrap().position, p1);
    undo.redo().unwrap();
    assert_eq!(g.edge_record(e[1]).unwrap().position, p2);
    assert_eq!(order(&g, n), vec![e[1], e[0], e[2], e[3]]);
    d.commit();
    check(&d, "g", &history(&d));
}

#[test]
fn one_group_spanning_an_independent_remote_map_import_undoes_both_local_moves() {
    let (a, n, e) = fixture(&["2080", "4080", "8080", "c080"]);
    let b = copy(&a, 23);
    let g = a.get_graph("g");
    let p0 = g.edge_record(e[1]).unwrap().position;
    let mut undo = UndoManager::new(&a);
    undo.set_merge_interval(0);
    undo.group_start().unwrap();
    g.reorder_edge(e[1], At::End).unwrap();
    a.commit(); // A1: first span of the explicit group.
    b.get_map("m").insert("independent", "remote").unwrap();
    b.commit();
    a.import(&b.export(loro::ExportMode::all_updates()).unwrap())
        .unwrap();
    g.reorder_edge(e[1], At::Start).unwrap();
    a.commit(); // A2 depends on the disjoint import and must split the local span.
    undo.group_end();
    let p2 = g.edge_record(e[1]).unwrap().position;
    assert_ne!(p2, p0);
    undo.undo().unwrap();
    assert_eq!(
        g.edge_record(e[1]).unwrap().position,
        p0,
        "A2 is in the same undo group; transforming split spans must not suppress A1"
    );
    assert_eq!(order(&g, n), e);
    assert_eq!(
        a.get_map("m").get("independent").unwrap().get_deep_value(),
        "remote".into()
    );
    undo.redo().unwrap();
    assert_eq!(g.edge_record(e[1]).unwrap().position, p2);
    assert_eq!(order(&g, n), vec![e[1], e[0], e[2], e[3]]);
    assert_eq!(
        a.get_map("m").get("independent").unwrap().get_deep_value(),
        "remote".into()
    );
}

#[test]
fn normal_copy_diff_can_be_undone_by_the_sources_reverse_diff() {
    let (source, n, e) = fixture(&["4080", "80", "c080"]);
    let g = source.get_graph("g");
    let p0 = g.edge_record(e[1]).unwrap().position;
    let before = source.state_frontiers();
    let target = copy(&source, 22);
    g.reorder_edge(e[1], At::End).unwrap();
    source.commit();
    let after = source.state_frontiers();
    let forward = source.diff(&before, &after).unwrap();
    let reverse = source.diff(&after, &before).unwrap();
    target.apply_diff(forward).unwrap();
    target.commit();
    let t = target.get_graph("g");
    assert_eq!(order(&t, n), vec![e[0], e[2], e[1]]);
    assert_eq!(
        t.edge_record(e[1]).unwrap().position,
        g.edge_record(e[1]).unwrap().position
    );
    assert_ne!(
        t.edge_record(e[1]).unwrap().last_order,
        g.edge_record(e[1]).unwrap().last_order
    );
    target.apply_diff(reverse).unwrap();
    target.commit();
    assert_eq!(
        t.edge_record(e[1]).unwrap().position,
        p0,
        "normal diff inversion must not require the source write to be the target's current writer"
    );
    assert_eq!(order(&t, n), e);
    check(&target, "g", &history(&target));
}

#[test]
fn undo_preserves_remote_winner_on_the_same_edge_and_remote_metadata() {
    let (seed, n, e) = fixture(&["2080", "4080", "8080", "c080"]);
    let a = copy(&seed, 2);
    let b = copy(&seed, 3);
    let mut undo = UndoManager::new(&a);
    undo.set_merge_interval(0);
    a.get_graph("g").reorder_edge(e[1], At::End).unwrap();
    a.commit();
    b.get_graph("g").reorder_edge(e[1], At::Start).unwrap();
    b.get_graph("g")
        .edge_meta(e[1])
        .unwrap()
        .insert("remote", "kept")
        .unwrap();
    b.commit();
    sync(&a, &b);
    let winner = a.get_graph("g").edge_record(e[1]).unwrap();
    assert_eq!(winner.last_order.peer, 3);
    undo.undo().unwrap();
    let g = a.get_graph("g");
    assert_eq!(g.edge_record(e[1]).unwrap().position, winner.position);
    assert_eq!(g.edge_record(e[1]).unwrap().last_order, winner.last_order);
    assert_eq!(order(&g, n), vec![e[1], e[0], e[2], e[3]]);
    assert_eq!(
        g.edge_meta(e[1])
            .unwrap()
            .get("remote")
            .unwrap()
            .get_deep_value(),
        "kept".into()
    );
}

#[test]
fn undo_reverts_local_order_and_property_without_overwriting_another_edge() {
    let (seed, n, e) = fixture(&["2080", "4080", "8080", "c080"]);
    let a = copy(&seed, 2);
    let b = copy(&seed, 3);
    let g = a.get_graph("g");
    g.edge_meta(e[0])
        .unwrap()
        .insert("local", "before")
        .unwrap();
    a.commit();
    let mut undo = UndoManager::new(&a);
    undo.set_merge_interval(0);
    g.reorder_edge(e[0], At::End).unwrap();
    g.edge_meta(e[0]).unwrap().insert("local", "after").unwrap();
    a.commit();
    b.get_graph("g").reorder_edge(e[3], At::Start).unwrap();
    b.get_graph("g")
        .node_meta(n)
        .unwrap()
        .insert("remote", 42)
        .unwrap();
    b.commit();
    sync(&a, &b);
    let remote = g.edge_record(e[3]).unwrap();
    undo.undo().unwrap();
    assert_eq!(order(&g, n), vec![e[3], e[0], e[1], e[2]]);
    assert_eq!(g.edge_record(e[3]).unwrap().last_order, remote.last_order);
    assert_eq!(
        g.edge_meta(e[0])
            .unwrap()
            .get("local")
            .unwrap()
            .get_deep_value(),
        "before".into()
    );
    assert_eq!(
        g.node_meta(n)
            .unwrap()
            .get("remote")
            .unwrap()
            .get_deep_value(),
        42.into()
    );
    undo.redo().unwrap();
    assert_eq!(order(&g, n), vec![e[3], e[1], e[2], e[0]]);
    assert_eq!(
        g.edge_meta(e[0])
            .unwrap()
            .get("local")
            .unwrap()
            .get_deep_value(),
        "after".into()
    );
    assert_eq!(g.edge_record(e[3]).unwrap().last_order, remote.last_order);
}

#[test]
fn auxiliary_undo_skips_only_the_edge_won_by_a_remote_write() {
    let (seed, n, e) = fixture(&["80", "80", "80", "c080"]);
    let a = copy(&seed, 2);
    let b = copy(&seed, 3);
    let g = a.get_graph("g");
    let original: Vec<_> = e
        .iter()
        .map(|id| g.edge_record(*id).unwrap().position)
        .collect();
    let mut undo = UndoManager::new(&a);
    undo.set_merge_interval(0);
    let outcome = g.reorder_edge(e[3], At::Before(e[1])).unwrap();
    assert!(outcome.changed && outcome.auxiliary_updates == 2);
    a.commit();
    for i in 0..12 {
        b.get_graph("g")
            .node_meta(n)
            .unwrap()
            .insert("clock", i)
            .unwrap();
    }
    b.get_graph("g").reorder_edge(e[1], At::Start).unwrap();
    b.commit();
    sync(&a, &b);
    let remote = g.edge_record(e[1]).unwrap();
    assert_eq!(remote.last_order.peer, 3);
    undo.undo().unwrap();
    assert_eq!(g.edge_record(e[1]).unwrap().position, remote.position);
    assert_eq!(g.edge_record(e[1]).unwrap().last_order, remote.last_order);
    for i in [0, 2, 3] {
        assert_eq!(g.edge_record(e[i]).unwrap().position, original[i]);
    }
    assert_eq!(order(&g, n), vec![e[1], e[0], e[2], e[3]]);
    // Redo must retain the same remote exclusion, while redoing local effects.
    undo.redo().unwrap();
    assert_eq!(g.edge_record(e[1]).unwrap().last_order, remote.last_order);
    assert_eq!(order(&g, n), vec![e[1], e[0], e[3], e[2]]);
    a.commit();
    check(&a, "g", &history(&a));
}
