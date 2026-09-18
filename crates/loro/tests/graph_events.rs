use std::borrow::Cow;
use std::sync::{Arc, Mutex};

use loro::event::{Diff, MapDelta};
use loro::{
    ApplyDiff, ContainerID, ContainerTrait, ExportMode, Index, LoroDoc, LoroGraph, LoroList,
    LoroMap, LoroText, LoroValue, Subscription, TreeID,
};

fn owned_diff(diff: Diff<'_>) -> Diff<'static> {
    match diff {
        Diff::Map(map) => Diff::Map(MapDelta {
            updated: map
                .updated
                .into_iter()
                .map(|(key, value)| (Cow::Owned(key.into_owned()), value))
                .collect(),
        }),
        Diff::Graph(graph) => Diff::Graph(Cow::Owned(graph.into_owned())),
        Diff::Tree(tree) => Diff::Tree(Cow::Owned(tree.into_owned())),
        Diff::Text(text) => Diff::Text(text),
        Diff::List(list) => Diff::List(list),
        #[cfg(feature = "counter")]
        Diff::Counter(counter) => Diff::Counter(counter),
        Diff::Unknown => panic!("unexpected unknown container"),
    }
}

struct MirrorState {
    value: LoroValue,
    paths: Vec<(ContainerID, Vec<Index>)>,
}

struct Mirror {
    state: Arc<Mutex<MirrorState>>,
    _subscription: Subscription,
}

impl Mirror {
    fn new(doc: &LoroDoc) -> Self {
        let state = Arc::new(Mutex::new(MirrorState {
            value: doc.get_deep_value(),
            paths: Vec::new(),
        }));
        let captured = state.clone();
        // The callback captures no document or handler. The initial deep value
        // and subsequent public event paths/diffs are its only state inputs.
        let subscription = doc.subscribe_root(Arc::new(move |event| {
            let mut state = captured.lock().unwrap();
            for item in event.events {
                let path: Vec<_> = item.path.iter().map(|(_, index)| index.clone()).collect();
                state
                    .value
                    .apply(&path.clone().into(), &[owned_diff(item.diff).into()]);
                state.paths.push((item.target.clone(), path));
            }
        }));
        Self {
            state,
            _subscription: subscription,
        }
    }

    fn check(&self, doc: &LoroDoc) {
        assert_eq!(self.state.lock().unwrap().value, doc.get_deep_value());
    }

    fn saw_path(&self, target: &ContainerID, path: &[Index]) -> bool {
        self.state
            .lock()
            .unwrap()
            .paths
            .iter()
            .any(|(id, actual)| id == target && actual == path)
    }
}

fn doc(peer: u64) -> LoroDoc {
    let doc = LoroDoc::new();
    doc.set_peer_id(peer).unwrap();
    doc
}

#[test]
fn node_and_edge_metadata_paths_rebuild_flat_rows() {
    let doc = doc(7);
    let graph = doc.get_graph("g");
    let a = graph.create_node().unwrap();
    let b = graph.create_node().unwrap();
    let edge = graph.create_edge(a, b).unwrap();
    let node_meta = graph.node_meta(a).unwrap();
    let edge_meta = graph.edge_meta(edge).unwrap();
    node_meta.insert("title", "old").unwrap();
    edge_meta.insert("weight", 1).unwrap();
    let node_text = node_meta.insert_container("body", LoroText::new()).unwrap();
    let edge_text = edge_meta.insert_container("body", LoroText::new()).unwrap();
    node_text.insert(0, "node").unwrap();
    edge_text.insert(0, "edge").unwrap();
    doc.commit();
    let mirror = Mirror::new(&doc);

    node_meta.insert("title", "new").unwrap();
    edge_meta.insert("weight", 2).unwrap();
    doc.commit();
    mirror.check(&doc);
    node_text.insert(0, "nested ").unwrap();
    edge_text.insert(0, "nested ").unwrap();
    doc.commit();
    mirror.check(&doc);

    let node_path = vec![
        Index::Key("g".into()),
        Index::Node(TreeID {
            peer: a.peer,
            counter: a.counter,
        }),
    ];
    let edge_path = vec![
        Index::Key("g".into()),
        Index::Node(TreeID {
            peer: edge.peer,
            counter: edge.counter,
        }),
    ];
    assert!(mirror.saw_path(&node_meta.id(), &node_path));
    assert!(mirror.saw_path(&edge_meta.id(), &edge_path));
    let mut text_path = edge_path.clone();
    text_path.push(Index::Key("body".into()));
    assert!(mirror.saw_path(&edge_text.id(), &text_path));
    assert_eq!(
        doc.get_by_path(&edge_path)
            .unwrap()
            .into_container()
            .unwrap()
            .id(),
        edge_meta.id()
    );
    text_path.push(Index::Seq(0));
    assert_eq!(
        doc.get_by_path(&text_path).unwrap().into_value().unwrap(),
        "n".into()
    );

    // A lower peer sorts before the existing rows. Metadata paths remain stable.
    doc.set_peer_id(1).unwrap();
    graph.create_node().unwrap();
    doc.commit();
    mirror.check(&doc);
    node_meta.delete("title").unwrap();
    edge_text.insert(0, "again ").unwrap();
    doc.commit();
    mirror.check(&doc);
    assert!(mirror.saw_path(&node_meta.id(), &node_path));
}

#[test]
fn hidden_node_and_edge_edits_are_replayed_on_restore() {
    let doc = doc(1);
    let graph = doc.get_graph("g");
    let a = graph.create_node().unwrap();
    let b = graph.create_node().unwrap();
    let edge = graph.create_edge(a, b).unwrap();
    let node_meta = graph.node_meta(a).unwrap();
    let edge_meta = graph.edge_meta(edge).unwrap();
    node_meta.insert("old", true).unwrap();
    edge_meta.insert("old", true).unwrap();
    let node_text = node_meta.insert_container("body", LoroText::new()).unwrap();
    let edge_text = edge_meta.insert_container("body", LoroText::new()).unwrap();
    node_text.insert(0, "node").unwrap();
    edge_text.insert(0, "edge").unwrap();
    doc.commit();
    let mirror = Mirror::new(&doc);

    graph.delete_node(a).unwrap();
    doc.commit();
    mirror.check(&doc);
    node_meta.delete("old").unwrap();
    node_meta.insert("title", "edited while hidden").unwrap();
    edge_meta.insert("weight", 42).unwrap();
    node_text.insert(0, "hidden ").unwrap();
    edge_text.insert(0, "hidden ").unwrap();
    doc.commit();
    mirror.check(&doc);
    graph.restore_node(a).unwrap();
    doc.commit();
    mirror.check(&doc);

    graph.delete_edge(edge).unwrap();
    doc.commit();
    mirror.check(&doc);
    edge_meta.delete("old").unwrap();
    edge_text.insert(0, "restored ").unwrap();
    graph.restore_edge(edge).unwrap();
    // Repeated disappearance/revival and a text edit in one transaction must
    // produce one replacement subtree rather than duplicate text insertions.
    graph.delete_edge(edge).unwrap();
    graph.restore_edge(edge).unwrap();
    doc.commit();
    mirror.check(&doc);
    edge_text.insert(0, "visible ").unwrap();
    doc.commit();
    mirror.check(&doc);
}

#[test]
fn checkout_revives_nested_graph_metadata_and_descendants() {
    let doc = doc(1);
    let parent = doc.get_map("parent");
    let graph = parent.insert_container("graph", LoroGraph::new()).unwrap();
    let node = graph.create_node().unwrap();
    let edge = graph.create_edge(node, node).unwrap();
    let node_meta = graph.node_meta(node).unwrap();
    let edge_meta = graph.edge_meta(edge).unwrap();
    node_meta.insert("title", "old node title").unwrap();
    edge_meta.insert("weight", 7).unwrap();
    let node_text = node_meta.insert_container("body", LoroText::new()).unwrap();
    let nested = edge_meta
        .insert_container("nested", LoroMap::new())
        .unwrap();
    let edge_text = nested.insert_container("body", LoroText::new()).unwrap();
    node_text.insert(0, "old node text").unwrap();
    edge_text.insert(0, "old edge text").unwrap();
    doc.commit();
    let present = doc.state_frontiers();
    let expected = doc.get_deep_value();
    parent.delete("graph").unwrap();
    doc.commit();
    let absent = doc.state_frontiers();
    let mirror = Mirror::new(&doc);

    // Only the parent Map changed between these versions. All metadata has to
    // arrive via revival events, including containers below the associated Map.
    for _ in 0..3 {
        doc.checkout(&present).unwrap();
        mirror.check(&doc);
        assert_eq!(mirror.state.lock().unwrap().value, expected);
        doc.checkout(&absent).unwrap();
        mirror.check(&doc);
    }
}

#[test]
fn import_and_checkout_rebuild_metadata_without_stale_keys() {
    let source = doc(1);
    let graph = source.get_graph("g");
    let node = graph.create_node().unwrap();
    let edge = graph.create_edge(node, node).unwrap();
    source.commit();
    let before = source.state_frontiers();
    let target = doc(2);
    target.get_graph("g");
    let mirror = Mirror::new(&target);
    target
        .import(&source.export(ExportMode::all_updates()).unwrap())
        .unwrap();
    mirror.check(&target);

    graph.delete_node(node).unwrap();
    source.commit();
    target
        .import(&source.export(ExportMode::all_updates()).unwrap())
        .unwrap();
    mirror.check(&target);
    graph.node_meta(node).unwrap().insert("later", 1).unwrap();
    let text = graph
        .edge_meta(edge)
        .unwrap()
        .insert_container("later", LoroText::new())
        .unwrap();
    text.insert(0, "later text").unwrap();
    source.commit();
    target
        .import(&source.export(ExportMode::all_updates()).unwrap())
        .unwrap();
    mirror.check(&target);
    graph.restore_node(node).unwrap();
    source.commit();
    target
        .import(&source.export(ExportMode::all_updates()).unwrap())
        .unwrap();
    mirror.check(&target);

    // The net topology is visible at both versions, but the old metadata state
    // has no keys at all (not even deletion tombstones).
    target.checkout(&before).unwrap();
    mirror.check(&target);
    target.checkout_to_latest();
    mirror.check(&target);
    assert_eq!(target.get_deep_value(), source.get_deep_value());
}

#[test]
fn ordinary_map_fields_are_not_interpreted_as_graph_rows() {
    let doc = doc(1);
    let map = doc.get_map("ordinary");
    let rows = map.insert_container("nodes", LoroList::new()).unwrap();
    map.insert_container("edges", LoroList::new()).unwrap();
    let row = rows.insert_container(0, LoroMap::new()).unwrap();
    row.insert("id", "0@1").unwrap();
    let meta = row.insert_container("meta", LoroMap::new()).unwrap();
    meta.insert("title", "old").unwrap();
    let cid_key = "cid:0@1:Map";
    let ordinary_child = map.insert_container(cid_key, LoroMap::new()).unwrap();
    doc.commit();
    let mirror = Mirror::new(&doc);

    meta.insert("title", "new").unwrap();
    ordinary_child.insert("title", "ordinary key").unwrap();
    doc.commit();
    mirror.check(&doc);
    assert!(mirror.saw_path(
        &meta.id(),
        &[
            Index::Key("ordinary".into()),
            Index::Key("nodes".into()),
            Index::Seq(0),
            Index::Key("meta".into()),
        ]
    ));
    assert!(mirror.saw_path(
        &ordinary_child.id(),
        &[Index::Key("ordinary".into()), Index::Key(cid_key.into()),]
    ));
}
