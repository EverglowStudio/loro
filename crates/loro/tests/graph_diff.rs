//! Public diff application is a local edit, with fresh operation identities.
use loro::{ExportMode, LoroDoc, UndoManager};

fn doc(peer: u64) -> LoroDoc {
    let doc = LoroDoc::new();
    doc.set_peer_id(peer).unwrap();
    doc
}

#[test]
fn compound_delete_restore_diff_is_correct_in_both_directions() {
    let source = doc(1);
    let graph = source.get_graph("g");
    let node = graph.create_node().unwrap();
    let edge = graph.create_edge(node, node).unwrap();
    source.commit();
    let a = source.state_frontiers();
    let initial = source.export(ExportMode::snapshot()).unwrap();
    graph.delete_node(node).unwrap();
    graph.delete_edge(edge).unwrap();
    graph.restore_node(node).unwrap();
    graph.restore_edge(edge).unwrap();
    source.commit();
    let b = source.state_frontiers();
    let expected = source.get_deep_value();

    let target = doc(2);
    target.import(&initial).unwrap();
    target.apply_diff(source.diff(&a, &b).unwrap()).unwrap();
    assert_eq!(target.get_deep_value(), expected);

    // Both endpoints of the historical interval are alive; applying its
    // inverse must not leave a synthetic Delete behind.
    source.apply_diff(source.diff(&b, &a).unwrap()).unwrap();
    assert_eq!(source.get_deep_value(), expected);
    assert!(graph.node_record(node).unwrap().delete_tags.is_empty());
    assert!(graph.edge_record(edge).unwrap().delete_tags.is_empty());
    source.revert_to(&a).unwrap();
    assert_eq!(source.get_deep_value(), expected);
}

#[test]
fn create_and_lifecycle_diff_remaps_nodes_edges_and_their_metadata() {
    for restore in [false, true] {
        let source = doc(1);
        let graph = source.get_graph("g");
        let a = source.state_frontiers();
        let node = graph.create_node().unwrap();
        let edge = graph.create_edge(node, node).unwrap();
        graph
            .node_meta(node)
            .unwrap()
            .insert("name", "node")
            .unwrap();
        graph
            .edge_meta(edge)
            .unwrap()
            .insert("name", "edge")
            .unwrap();
        graph.delete_node(node).unwrap();
        graph.delete_edge(edge).unwrap();
        if restore {
            graph.restore_node(node).unwrap();
            graph.restore_edge(edge).unwrap();
        }
        source.commit();
        let target = doc(2);
        target
            .apply_diff(source.diff(&a, &source.state_frontiers()).unwrap())
            .unwrap();
        let copy = target.get_graph("g");
        let nodes = copy.node_records();
        let edges = copy.edge_records();
        assert_eq!(nodes.len(), 1);
        assert_eq!(edges.len(), 1);
        assert_ne!(nodes[0].id, node);
        assert_ne!(edges[0].id, edge);
        assert_eq!(edges[0].source, nodes[0].id);
        assert_eq!(edges[0].target, nodes[0].id);
        assert_eq!(nodes[0].alive, restore);
        assert_eq!(edges[0].alive, restore);
        assert_eq!(
            copy.node_meta(nodes[0].id)
                .unwrap()
                .get("name")
                .unwrap()
                .get_deep_value(),
            "node".into()
        );
        assert_eq!(
            copy.edge_meta(edges[0].id)
                .unwrap()
                .get("name")
                .unwrap()
                .get_deep_value(),
            "edge".into()
        );
    }
}

#[test]
fn inverse_diff_and_selective_undo_keep_independent_remote_deletes() {
    let source = doc(1);
    let graph = source.get_graph("g");
    let node = graph.create_node().unwrap();
    source.commit();
    let initial = source.export(ExportMode::snapshot()).unwrap();
    let a = source.state_frontiers();
    let mut undo = UndoManager::new(&source);
    undo.set_merge_interval(0);
    graph.delete_node(node).unwrap();
    graph.restore_node(node).unwrap();
    source.commit();
    let b = source.state_frontiers();
    let inverse = source.diff(&b, &a).unwrap();
    let remote = doc(2);
    remote.import(&initial).unwrap();
    remote.get_graph("g").delete_node(node).unwrap();
    source
        .import(&remote.export(ExportMode::all_updates()).unwrap())
        .unwrap();
    let independent = graph.node_record(node).unwrap().delete_tags;
    assert_eq!(independent.len(), 1);
    source.apply_diff(inverse).unwrap();
    assert_eq!(graph.node_record(node).unwrap().delete_tags, independent);
    // This local interval has no net lifecycle effect, so undo can be a no-op.
    undo.undo().unwrap();
    assert_eq!(graph.node_record(node).unwrap().delete_tags, independent);
    assert!(!graph.node_record(node).unwrap().alive);
}

#[test]
fn undo_one_restore_keeps_an_independent_restore_of_the_same_delete() {
    let source = doc(1);
    let graph = source.get_graph("g");
    let node = graph.create_node().unwrap();
    graph.delete_node(node).unwrap();
    source.commit();
    let remote = doc(2);
    remote
        .import(&source.export(ExportMode::snapshot()).unwrap())
        .unwrap();
    let mut undo = UndoManager::new(&source);
    undo.set_merge_interval(0);
    graph.restore_node(node).unwrap();
    source.commit();
    remote.get_graph("g").restore_node(node).unwrap();
    source
        .import(&remote.export(ExportMode::all_updates()).unwrap())
        .unwrap();
    undo.undo().unwrap();
    assert!(graph.node_record(node).unwrap().alive);
    assert!(graph.node_record(node).unwrap().delete_tags.is_empty());
}

#[test]
fn lifecycle_undo_preserves_independent_remote_metadata() {
    for restore_in_interval in [false, true] {
        for edit_property in [false, true] {
            let source = doc(1);
            let graph = source.get_graph("g");
            let node = graph.create_node().unwrap();
            let edge = graph.create_edge(node, node).unwrap();
            let node_meta = graph.node_meta(node).unwrap();
            let edge_meta = graph.edge_meta(edge).unwrap();
            for meta in [&node_meta, &edge_meta] {
                meta.insert("title", "base").unwrap();
                meta.insert("local", "before").unwrap();
            }
            source.commit();
            let remote = doc(2);
            remote
                .import(&source.export(ExportMode::snapshot()).unwrap())
                .unwrap();
            let before = source.state_frontiers();
            let mut undo = UndoManager::new(&source);
            undo.set_merge_interval(0);
            graph.delete_node(node).unwrap();
            graph.delete_edge(edge).unwrap();
            if restore_in_interval {
                graph.restore_node(node).unwrap();
                graph.restore_edge(edge).unwrap();
            }
            if edit_property {
                node_meta.insert("local", "after").unwrap();
                edge_meta.insert("local", "after").unwrap();
            }
            source.commit();
            let after = source.state_frontiers();
            let inverse = source.diff(&after, &before).unwrap();
            let remote_graph = remote.get_graph("g");
            remote_graph
                .node_meta(node)
                .unwrap()
                .insert("title", "remote")
                .unwrap();
            remote_graph
                .edge_meta(edge)
                .unwrap()
                .insert("title", "remote")
                .unwrap();
            source
                .import(&remote.export(ExportMode::all_updates()).unwrap())
                .unwrap();
            let applied = source.fork();
            undo.undo().unwrap();
            applied.apply_diff(inverse).unwrap();
            for target in [&source, &applied] {
                let graph = target.get_graph("g");
                assert!(graph.get_node(node).is_some());
                assert!(graph.get_edge(edge).is_some());
                for meta in [
                    graph.node_meta(node).unwrap(),
                    graph.edge_meta(edge).unwrap(),
                ] {
                    assert_eq!(
                        meta.get("title").unwrap().get_deep_value(),
                        "remote".into(),
                        "restore={restore_in_interval}, property_edit={edit_property}"
                    );
                    assert_eq!(meta.get("local").unwrap().get_deep_value(), "before".into());
                }
            }
        }
    }
}
