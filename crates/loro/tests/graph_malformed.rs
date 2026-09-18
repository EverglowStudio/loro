//! Invalid external graph operations must fail atomically, without poisoning a doc.
use loro::{ExportMode, GraphNodeId, JsonOpContent, JsonSchema, LoroDoc, VersionVector, ID};
use loro_internal::container::graph::GraphOp;

fn json(doc: &LoroDoc) -> JsonSchema {
    doc.export_json_updates_without_peer_compression(&VersionVector::default(), &doc.oplog_vv())
}
fn assert_rejected(schema: JsonSchema) {
    let doc = LoroDoc::new();
    doc.get_map("existing").insert("keep", true).unwrap();
    doc.commit();
    let version = doc.oplog_vv();
    let state = doc.get_deep_value();
    assert!(doc.import_json_updates(schema).is_err());
    assert_eq!(doc.oplog_vv(), version);
    assert_eq!(doc.get_deep_value(), state);
    doc.get_map("existing").insert("usable", true).unwrap();
    doc.commit();
}

#[test]
fn invalid_creation_ids_missing_endpoints_and_cross_graph_endpoints_are_atomic_errors() {
    let source = LoroDoc::new();
    source.set_peer_id(1).unwrap();
    let g = source.get_graph("g");
    let a = g.create_node().unwrap();
    let b = g.create_node().unwrap();
    let other = source.get_graph("other").create_node().unwrap();
    g.create_edge(a, b).unwrap();
    source.commit();
    let original = json(&source);
    for mode in 0..3 {
        let mut bad = original.clone();
        for change in &mut bad.changes {
            for op in &mut change.ops {
                if let JsonOpContent::Graph(graph) = &mut op.content {
                    match graph {
                        GraphOp::CreateNode { id } if mode == 0 && *id == a => {
                            *id = GraphNodeId::new(88, 999)
                        }
                        GraphOp::CreateEdge { source, .. } if mode == 1 => {
                            *source = GraphNodeId::new(88, 999)
                        }
                        GraphOp::CreateEdge { source, .. } if mode == 2 => *source = other,
                        _ => {}
                    }
                }
            }
        }
        assert_rejected(bad);
    }
    let restored = LoroDoc::new();
    restored.import_json_updates(original).unwrap();
    assert_eq!(restored.get_deep_value(), source.get_deep_value());
}

#[test]
fn restore_cannot_claim_a_concurrent_delete_even_if_receiver_has_it() {
    let seed = LoroDoc::new();
    seed.set_peer_id(1).unwrap();
    let n = seed.get_graph("g").create_node().unwrap();
    seed.get_graph("g").delete_node(n).unwrap();
    seed.commit();
    let a = LoroDoc::new();
    a.set_peer_id(2).unwrap();
    a.import(&seed.export(ExportMode::snapshot()).unwrap())
        .unwrap();
    let b = LoroDoc::new();
    b.set_peer_id(3).unwrap();
    b.import(&seed.export(ExportMode::snapshot()).unwrap())
        .unwrap();
    a.get_graph("g").delete_node(n).unwrap();
    a.commit();
    b.get_graph("g").restore_node(n).unwrap();
    b.commit();
    let delete_id = json(&a)
        .changes
        .iter()
        .flat_map(|change| change.ops.iter().map(move |op| (change.id.peer, op)))
        .find_map(|(peer, op)| {
            (peer == 2 && matches!(op.content, JsonOpContent::Graph(GraphOp::DeleteNode { .. })))
                .then_some(ID::new(peer, op.counter))
        })
        .unwrap();
    let mut forged = json(&b);
    for change in &mut forged.changes {
        for op in &mut change.ops {
            if let JsonOpContent::Graph(GraphOp::RestoreNode { deletes, .. }) = &mut op.content {
                deletes.push(delete_id);
            }
        }
    }
    let before = a.get_deep_value();
    let version = a.oplog_vv();
    assert!(a.import_json_updates(forged).is_err());
    assert_eq!(a.get_deep_value(), before);
    assert_eq!(a.oplog_vv(), version);
    a.import(&b.export(ExportMode::all_updates()).unwrap())
        .unwrap();
    assert!(!a.get_graph("g").node_record(n).unwrap().alive);
}

#[test]
fn graph_payload_rejects_truncation_trailing_data_and_negative_identity() {
    let id = ID::new(1, 0);
    let op = GraphOp::CreateNode {
        id: GraphNodeId::from_id(id),
    };
    let bytes = op.encoded();
    assert_eq!(GraphOp::decode(&bytes, id).unwrap(), op);
    for end in 0..bytes.len() {
        assert!(GraphOp::decode(&bytes[..end], id).is_err(), "prefix {end}");
    }
    let mut trailing = bytes.clone();
    trailing.push(0);
    assert!(GraphOp::decode(&trailing, id).is_err());
    assert!(GraphOp::decode(&bytes, ID::new(1, -1)).is_err());
    let mut invalid = bytes;
    invalid[0] = 255;
    assert!(GraphOp::decode(&invalid, id).is_err());
}
