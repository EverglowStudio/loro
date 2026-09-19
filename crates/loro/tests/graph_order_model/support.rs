//! Public API adapters and fixtures. Expected registers come only from oracle.rs.
use super::oracle::{History, Id, Snapshot};
use loro::{
    ContainerTrait, ExportMode, GraphEdgeId, GraphNodeId, LoroDoc, LoroGraph, VersionVector,
};
use serde_json::Value;

pub fn doc(peer: u64) -> LoroDoc {
    let doc = LoroDoc::new();
    doc.set_peer_id(peer).unwrap();
    doc
}
pub fn node(id: Id) -> GraphNodeId {
    GraphNodeId::new(id.peer, id.counter)
}
pub fn edge(id: Id) -> GraphEdgeId {
    GraphEdgeId::new(id.peer, id.counter)
}
pub fn eid(id: GraphEdgeId) -> Id {
    Id {
        peer: id.peer,
        counter: id.counter,
    }
}
pub fn nid(id: GraphNodeId) -> Id {
    Id {
        peer: id.peer,
        counter: id.counter,
    }
}

pub fn json_between(doc: &LoroDoc, start: &VersionVector, end: &VersionVector) -> Value {
    serde_json::to_value(doc.export_json_updates_without_peer_compression(start, end)).unwrap()
}
pub fn json(doc: &LoroDoc) -> Value {
    json_between(doc, &VersionVector::default(), &doc.state_vv())
}
pub fn history(doc: &LoroDoc) -> History {
    History::from_json(&json(doc))
}
pub fn import_json(doc: &LoroDoc, value: Value) {
    doc.import_json_updates(serde_json::from_value::<loro::JsonSchema>(value).unwrap())
        .unwrap();
}
pub fn copy(source: &LoroDoc, peer: u64) -> LoroDoc {
    let target = doc(peer);
    target
        .import(&source.export(ExportMode::snapshot()).unwrap())
        .unwrap();
    target
}
pub fn sync(a: &LoroDoc, b: &LoroDoc) {
    b.import(&a.export(ExportMode::all_updates()).unwrap())
        .unwrap();
    a.import(&b.export(ExportMode::all_updates()).unwrap())
        .unwrap();
}
pub fn order(g: &LoroGraph, source: GraphNodeId) -> Vec<GraphEdgeId> {
    g.ordered_out_edges(source)
        .unwrap()
        .into_iter()
        .map(|e| e.edge_id)
        .collect()
}
pub fn check(doc: &LoroDoc, name: &str, expected: &History) {
    let g = doc.get_graph(name);
    check_snapshot(&g, &expected.snapshot(&g.id().to_string()));
}
pub fn check_snapshot(g: &LoroGraph, expected: &Snapshot) {
    assert_eq!(g.node_records().len(), expected.nodes.len());
    assert_eq!(g.edge_records().len(), expected.edges.len());
    for (id, alive) in &expected.nodes {
        let actual = g.node_record(node(*id)).unwrap();
        assert_eq!((actual.alive, actual.visible), (*alive, *alive));
        let ordered = expected.ordered(*id);
        let actual = g.ordered_out_edges(node(*id)).unwrap();
        assert_eq!(
            actual.iter().map(|e| eid(e.edge_id)).collect::<Vec<_>>(),
            ordered
        );
        for (index, row) in actual.iter().enumerate() {
            let record = &expected.edges[&eid(row.edge_id)];
            assert_eq!(nid(row.target), record.target);
            assert_eq!(row.position.as_bytes(), record.position);
            assert_eq!(g.out_edge_at(node(*id), index).as_ref(), Some(row));
            assert_eq!(g.index_of_out_edge(row.edge_id), Some(index));
        }
        assert!(g.out_edge_at(node(*id), ordered.len()).is_none());
        assert!(g.out_edge_at(node(*id), usize::MAX).is_none());
    }
    for (id, expected) in &expected.edges {
        let actual = g.edge_record(edge(*id)).unwrap();
        assert_eq!(
            (nid(actual.source), nid(actual.target)),
            (expected.source, expected.target)
        );
        assert_eq!(
            (actual.alive, actual.visible),
            (expected.alive, expected.visible)
        );
        assert_eq!(actual.position.as_bytes(), expected.position);
        assert_eq!(
            (
                actual.last_order.peer,
                actual.last_order.counter,
                u64::from(actual.last_order.lamport)
            ),
            (
                expected.writer.peer,
                expected.writer.counter,
                expected.lamport
            )
        );
        if !expected.visible {
            assert_eq!(g.index_of_out_edge(edge(*id)), None);
        }
    }
}

/// Change only canonical position bytes in a genuine, causally valid history.
/// Operation identities, endpoint references, Lamports and dependencies are intact.
pub fn fixture(keys: &[&str]) -> (LoroDoc, GraphNodeId, Vec<GraphEdgeId>) {
    let source = doc(7);
    let g = source.get_graph("g");
    g.configure_order_jitter(0);
    let parent = g.create_node().unwrap();
    let edges: Vec<_> = keys
        .iter()
        .map(|_| g.create_edge(parent, parent).unwrap())
        .collect();
    source.commit();
    let mut schema = json(&source);
    let mut replaced = 0;
    for change in schema["changes"].as_array_mut().unwrap() {
        for op in change["ops"].as_array_mut().unwrap() {
            if let Some(body) = op["content"].get_mut("create_edge") {
                assert_eq!(Id::parse(&body["id"]), eid(edges[replaced]));
                super::oracle::decode_hex(&Value::String(keys[replaced].into()));
                body["position"] = keys[replaced].into();
                replaced += 1;
            }
        }
    }
    assert_eq!(replaced, keys.len());
    let result = doc(11);
    import_json(&result, schema);
    check(&result, "g", &history(&result));
    (result, parent, edges)
}
