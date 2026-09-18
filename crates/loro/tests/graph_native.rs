use loro::{
    ContainerTrait, ContainerType, ExportMode, Frontiers, GraphEdgeId, GraphNodeId, LoroDoc,
    LoroGraph, LoroText, UndoManager,
};

fn sync(a: &LoroDoc, b: &LoroDoc) {
    b.import(&a.export(ExportMode::all_updates()).unwrap())
        .unwrap();
    a.import(&b.export(ExportMode::all_updates()).unwrap())
        .unwrap();
}
fn doc(peer: u64) -> LoroDoc {
    let d = LoroDoc::new();
    d.set_peer_id(peer).unwrap();
    d
}

#[test]
fn native_multigraph_identity_metadata_and_scope() {
    let d = doc(1);
    let g = d.get_graph("g");
    let a = g.create_node().unwrap();
    let b = g.create_node().unwrap();
    let e = g.create_edge(a, b).unwrap();
    let p = g.create_edge(a, b).unwrap();
    let back = g.create_edge(b, a).unwrap();
    g.create_edge(a, a).unwrap();
    assert_eq!(g.node_count(), 2);
    assert_eq!(g.edge_count(), 4);
    assert_eq!(g.successors(a).unwrap(), vec![a, b]);
    assert_ne!(a.id(), e.id());
    assert_ne!(g.node_meta(a).unwrap().id(), g.edge_meta(e).unwrap().id());
    assert!(d.get_graph("other").create_edge(a, b).is_err());
    assert!(g.delete_edge(GraphEdgeId::new(99, 0)).is_err());
    assert!(g.create_edge(GraphNodeId::new(99, 0), b).is_err());
    let text = g
        .node_meta(a)
        .unwrap()
        .insert_container("text", LoroText::new())
        .unwrap();
    text.insert(0, "hi").unwrap();
    g.delete_edge(e).unwrap();
    assert_eq!(g.outgoing_edges(a).unwrap().len(), 2);
    assert!(g.get_edge(p).is_some());
    g.delete_node(a).unwrap();
    assert_eq!(g.nodes(), vec![b]);
    assert_eq!(g.edge_count(), 0);
    assert!(g.edge_record(back).unwrap().alive);
    assert!(!g.edge_record(back).unwrap().visible);
    g.restore_node(a).unwrap();
    assert_eq!(g.edge_count(), 3);
    assert!(g.get_edge(e).is_none());
    assert_eq!(text.to_string(), "hi");
    assert_eq!(
        g.get_deep_value(),
        d.get_deep_value().as_map().unwrap()["g"]
    );
    assert_eq!(ContainerType::Graph.to_u8(), 6);
    assert_eq!(g.traverse(a, 20, 2).unwrap().len(), 2);
}
#[test]
fn remove_wins_restore_and_concurrent_edge() {
    let a = doc(1);
    let g = a.get_graph("g");
    let n = g.create_node().unwrap();
    let m = g.create_node().unwrap();
    a.commit();
    let b = doc(2);
    sync(&a, &b);
    let h = b.get_graph("g");
    g.delete_node(n).unwrap();
    let edge = h.create_edge(m, n).unwrap();
    h.node_meta(n).unwrap().insert("remote", true).unwrap();
    sync(&a, &b);
    assert!(!g.node_record(n).unwrap().alive);
    assert!(g.edge_record(edge).unwrap().alive);
    assert!(!g.edge_record(edge).unwrap().visible);
    g.restore_node(n).unwrap();
    h.delete_node(n).unwrap();
    sync(&a, &b);
    assert!(!g.node_record(n).unwrap().alive);
    g.restore_node(n).unwrap();
    sync(&a, &b);
    assert!(g.get_edge(edge).is_some());
    assert_eq!(g.get_deep_value(), h.get_deep_value());
}
#[test]
fn binary_json_snapshot_and_history() {
    let d = doc(1);
    let g = d.get_graph("g");
    let n = g.create_node().unwrap();
    let m = g.create_node().unwrap();
    let e = g.create_edge(n, m).unwrap();
    g.node_meta(n).unwrap().insert("k", 42).unwrap();
    d.commit();
    let before = d.state_frontiers();
    g.delete_node(n).unwrap();
    d.commit();
    let deleted = d.state_frontiers();
    g.restore_node(n).unwrap();
    d.commit();
    let end = d.state_frontiers();
    for mode in [
        ExportMode::Snapshot,
        ExportMode::all_updates(),
        ExportMode::shallow_snapshot(&deleted),
        ExportMode::StateOnly(None),
    ] {
        let b = doc(2);
        b.import(&d.export(mode).unwrap()).unwrap();
        assert_eq!(g.get_deep_value(), b.get_graph("g").get_deep_value());
        let h = b.get_graph("g");
        h.delete_node(n).unwrap();
        h.restore_node(n).unwrap();
        assert!(h.get_edge(e).is_some());
    }
    let b = doc(2);
    b.import_json_updates(d.export_json_updates(&Default::default(), &d.oplog_vv()))
        .unwrap();
    assert_eq!(g.get_deep_value(), b.get_graph("g").get_deep_value());
    d.checkout(&before).unwrap();
    assert!(g.get_edge(e).is_some());
    d.checkout(&deleted).unwrap();
    assert!(g.get_edge(e).is_none());
    d.checkout(&end).unwrap();
    assert!(g.get_edge(e).is_some());
    d.checkout(&Frontiers::default()).unwrap();
    assert_eq!(g.node_count(), 0);
    d.checkout_to_latest();
    assert!(g.get_edge(e).is_some());
}
#[test]
fn snapshot_merge_unlocks_pending_graph_changes() {
    let source = doc(1);
    let graph = source.get_graph("g");
    let a = graph.create_node().unwrap();
    let b = graph.create_node().unwrap();
    source.commit();
    let base_frontiers = source.state_frontiers();
    let base_vv = source.oplog_vv();
    let edge = graph.create_edge(a, b).unwrap();
    let edge_update = source.export(ExportMode::updates(&base_vv)).unwrap();
    let edge_vv = source.oplog_vv();
    graph.delete_node(a).unwrap();
    let delete_update = source.export(ExportMode::updates(&edge_vv)).unwrap();

    let fork = source.fork_at(&base_frontiers).unwrap();
    fork.set_peer_id(100).unwrap();
    let fork_graph = fork.get_graph("g");
    let reverse = fork_graph.create_edge(b, a).unwrap();
    fork_graph
        .node_meta(a)
        .unwrap()
        .insert("branch", "historical edit")
        .unwrap();
    let snapshot = fork.export(ExportMode::Snapshot).unwrap();

    let target = doc(3);
    for packet in [&delete_update, &edge_update, &delete_update] {
        let status = target.import(packet).unwrap();
        assert!(status.success.is_empty());
        assert!(status.pending.is_some());
    }
    assert!(target.oplog_vv().is_empty());
    let status = target.import(&snapshot).unwrap();
    assert!(status.pending.is_none());
    assert_eq!(target.oplog_vv().get(&1), Some(&4));
    assert_eq!(target.oplog_vv().get(&100), Some(&2));
    assert_eq!(target.state_frontiers(), target.oplog_frontiers());

    source.import(&snapshot).unwrap();
    assert_eq!(target.oplog_vv(), source.oplog_vv());
    assert_eq!(target.get_deep_value(), source.get_deep_value());
    let merged = target.get_graph("g");
    assert!(!merged.node_record(a).unwrap().alive);
    for id in [edge, reverse] {
        let record = merged.edge_record(id).unwrap();
        assert!(record.alive);
        assert!(!record.visible);
    }
    merged.restore_node(a).unwrap();
    assert_eq!(merged.edge_count(), 2);
}

#[test]
fn pending_delete_is_activated_by_a_one_node_snapshot() {
    // Reduced seed-61516 schedule: one node, one pending delete, two replicas.
    let source = doc(1);
    let node = source.get_graph("g").create_node().unwrap();
    let snapshot = source.export(ExportMode::snapshot()).unwrap();
    let base = source.oplog_vv();
    source.get_graph("g").delete_node(node).unwrap();
    let deletion = source.export(ExportMode::updates(&base)).unwrap();
    let target = doc(2);
    assert!(target.import(&deletion).unwrap().pending.is_some());
    assert!(target.oplog_vv().is_empty());
    assert!(target.import(&snapshot).unwrap().pending.is_none());
    assert_eq!(target.oplog_vv(), source.oplog_vv());
    assert!(!target.get_graph("g").node_record(node).unwrap().alive);
    assert_eq!(target.get_deep_value(), source.get_deep_value());
}

#[test]
fn selective_undo_preserves_independent_delete() {
    let a = doc(1);
    let g = a.get_graph("g");
    let n = g.create_node().unwrap();
    a.commit();
    let b = doc(2);
    sync(&a, &b);
    let mut undo = UndoManager::new(&a);
    undo.set_merge_interval(0);
    g.delete_node(n).unwrap();
    a.commit();
    b.get_graph("g").delete_node(n).unwrap();
    sync(&a, &b);
    assert!(undo.undo().unwrap());
    assert!(!g.node_record(n).unwrap().alive);
    assert_eq!(g.node_record(n).unwrap().delete_tags.len(), 1);
    assert!(undo.redo().unwrap());
    assert!(!g.node_record(n).unwrap().alive);
}
#[test]
fn detached_nested_and_fork() {
    let g = LoroGraph::new();
    let n = g.create_node().unwrap();
    let e = g.create_edge(n, n).unwrap();
    g.edge_meta(e).unwrap().insert("a", 7).unwrap();
    let d = doc(1);
    let g = d.get_map("m").insert_container("graph", g).unwrap();
    assert_eq!(g.node_count(), 1);
    assert_eq!(g.edge_count(), 1);
    d.commit();
    let f = d.fork();
    assert_eq!(f.get_deep_value(), d.get_deep_value());
    assert_eq!(
        d.get_deep_value().as_map().unwrap()["m"].as_map().unwrap()["graph"],
        g.get_deep_value()
    );
}

#[test]
fn malformed_references_reject_atomically_and_pending_is_checked() {
    use loro::{JsonOpContent, ID};
    use loro_internal::container::graph::GraphOp;
    let a = doc(1);
    let g = a.get_graph("g");
    let n = g.create_node().unwrap();
    let m = g.create_node().unwrap();
    g.delete_node(n).unwrap();
    g.delete_node(m).unwrap();
    g.restore_node(n).unwrap();
    a.commit();
    let schema = a.export_json_updates_without_peer_compression(&Default::default(), &a.oplog_vv());
    for bad in [ID::new(99, 0), ID::new(1, 3), ID::new(1, 0), ID::new(1, 50)] {
        let mut bad_schema = schema.clone();
        for change in &mut bad_schema.changes {
            for op in &mut change.ops {
                if let JsonOpContent::Graph(GraphOp::RestoreNode { deletes, .. }) = &mut op.content
                {
                    *deletes = vec![bad];
                }
            }
        }
        let b = doc(2);
        assert!(b.import_json_updates(bad_schema).is_err());
        assert!(b.oplog_vv().is_empty());
        assert_eq!(b.get_graph("g").node_count(), 0);
        b.import_json_updates(schema.clone()).unwrap();
        assert_eq!(g.get_deep_value(), b.get_graph("g").get_deep_value());
    }
    // Both creations exist, but this forged second-peer operation has not observed them.
    let mut bad = schema.clone();
    bad.changes.retain(|c| c.id.peer == 1);
    let mut change = bad.changes[0].clone();
    change.id = ID::new(3, 0);
    change.deps.clear();
    change.ops = vec![loro::JsonOp {
        counter: 0,
        container: g.id(),
        content: JsonOpContent::Graph(GraphOp::DeleteNode { id: n }),
    }];
    bad.changes.push(change);
    let b = doc(2);
    assert!(b.import_json_updates(bad).is_err());
    assert!(b.oplog_vv().is_empty());
    let mut orphan = schema.clone();
    let last = orphan.changes.last_mut().unwrap();
    last.ops = vec![loro::JsonOp {
        counter: 0,
        container: g.id(),
        content: JsonOpContent::Graph(GraphOp::RestoreNode {
            id: n,
            deletes: vec![ID::new(1, 0)],
        }),
    }];
    last.id = ID::new(3, 0);
    last.deps = vec![ID::new(1, 0)];
    orphan.changes = vec![last.clone()];
    let b = doc(2);
    let status = b.import_json_updates(orphan).unwrap();
    assert!(status.pending.is_some());
    assert!(b.import_json_updates(schema).is_err());
    assert!(b.oplog_vv().is_empty());
}

#[test]
fn undo_redo_create_delete_restore_and_metadata() {
    let d = doc(1);
    let g = d.get_graph("g");
    let mut undo = UndoManager::new(&d);
    undo.set_merge_interval(0);
    let n = g.create_node().unwrap();
    g.node_meta(n).unwrap().insert("value", 1).unwrap();
    let e = g.create_edge(n, n).unwrap();
    d.commit();
    assert!(undo.undo().unwrap());
    assert_eq!(g.node_count(), 0);
    assert!(undo.redo().unwrap());
    assert_eq!(g.node_count(), 1);
    assert!(g.get_node(n).is_some());
    assert!(g.get_edge(e).is_some());
    assert_eq!(
        g.node_meta(n)
            .unwrap()
            .get("value")
            .unwrap()
            .get_deep_value(),
        1.into()
    );
    g.delete_node(n).unwrap();
    d.commit();
    g.restore_node(n).unwrap();
    d.commit();
    assert!(undo.undo().unwrap());
    assert!(g.get_node(n).is_none());
    assert!(undo.redo().unwrap());
    assert!(g.get_node(n).is_some());
}
