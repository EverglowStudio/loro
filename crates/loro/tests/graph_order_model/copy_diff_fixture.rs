//! Public operations create two original equal-key groups with interleaved IDs.
use super::support::*;
use loro::{ExportMode, Frontiers, GraphEdgeId, GraphNodeId, LoroDoc, LoroGraph};

pub fn named(g: &LoroGraph, parent: GraphNodeId, name: &str) -> GraphEdgeId {
    let id = g.create_edge(parent, parent).unwrap();
    g.edge_meta(id).unwrap().insert("name", name).unwrap();
    id
}

pub fn name(g: &LoroGraph, id: GraphEdgeId) -> String {
    let value = g
        .edge_meta(id)
        .unwrap()
        .get("name")
        .unwrap()
        .get_deep_value();
    value.as_string().unwrap().to_string()
}

pub fn names(g: &LoroGraph, parent: GraphNodeId) -> Vec<String> {
    order(g, parent).into_iter().map(|id| name(g, id)).collect()
}

pub fn find(g: &LoroGraph, label: &str) -> GraphEdgeId {
    g.edge_records()
        .into_iter()
        .find(|record| name(g, record.id) == label)
        .unwrap()
        .id
}

pub struct Groups {
    pub source: LoroDoc,
    pub target: LoroDoc,
    pub v0: Frontiers,
    pub parent: GraphNodeId,
    /// Source logical order: G1 prefix, G1 copy, G1 suffix, G2 copy, G2 existing.
    pub ids: [GraphEdgeId; 5],
}

pub fn groups(hidden_suffix_at_base: bool) -> Groups {
    let seed = doc(1);
    let parent = seed.get_graph("g").create_node().unwrap();
    seed.commit();
    let left = copy(&seed, 2);
    let prefix = named(&left.get_graph("g"), parent, "g1-prefix");
    left.commit();
    let source = copy(&seed, 4);
    let copied1 = named(&source.get_graph("g"), parent, "g1-copy");
    let copied2 = named(&source.get_graph("g"), parent, "g2-copy");
    source.commit();
    let right = copy(&seed, 9);
    let suffix = named(&right.get_graph("g"), parent, "g1-suffix");
    let existing2 = named(&right.get_graph("g"), parent, "g2-existing");
    if hidden_suffix_at_base {
        right.get_graph("g").delete_edge(suffix).unwrap();
    }
    right.commit();
    right
        .import(&left.export(ExportMode::all_updates()).unwrap())
        .unwrap();
    let v0 = right.state_frontiers();
    let target = copy(&right, 20);
    source
        .import(&right.export(ExportMode::all_updates()).unwrap())
        .unwrap();
    let g = source.get_graph("g");
    let k1 = g.edge_record(prefix).unwrap().position;
    let k2 = g.edge_record(existing2).unwrap().position;
    assert!(k1 < k2);
    for id in [copied1, suffix] {
        assert_eq!(g.edge_record(id).unwrap().position, k1);
    }
    assert_eq!(g.edge_record(copied2).unwrap().position, k2);
    Groups {
        source,
        target,
        v0,
        parent,
        ids: [prefix, copied1, suffix, copied2, existing2],
    }
}
