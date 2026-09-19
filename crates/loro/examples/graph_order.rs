//! Two parents order relations to the same children independently while offline.
use loro::{ExportMode, GraphOrderTarget, LoroDoc};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let a = LoroDoc::new();
    a.set_peer_id(1)?;
    let graph = a.get_graph("relations");
    let p = graph.create_node()?;
    let q = graph.create_node()?;
    let x = graph.create_node()?;
    let y = graph.create_node()?;
    let z = graph.create_node()?;
    let px = graph.create_edge(p, x)?;
    let py = graph.create_edge(p, y)?;
    let pz = graph.create_edge(p, z)?;
    let qz = graph.create_edge(q, z)?;
    let qx = graph.create_edge(q, x)?;
    let qy = graph.create_edge(q, y)?;
    graph.node_meta(x)?.insert("content", "one shared node")?;
    a.commit();
    let b = a.fork();
    b.set_peer_id(2)?;

    graph.reorder_edge(px, GraphOrderTarget::End)?;
    b.get_graph("relations")
        .reorder_edge(qy, GraphOrderTarget::Start)?;
    a.commit();
    b.commit();
    a.import(&b.export(ExportMode::all_updates())?)?;
    b.import(&a.export(ExportMode::all_updates())?)?;

    for doc in [&a, &b] {
        let graph = doc.get_graph("relations");
        let ids = |parent| {
            graph
                .ordered_out_edges(parent)
                .unwrap()
                .into_iter()
                .map(|edge| edge.edge_id)
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(p), [py, pz, px]);
        assert_eq!(ids(q), [qy, qz, qx]);
        assert_eq!(graph.node_count(), 5);
        assert_eq!(graph.edge_count(), 6);
    }
    assert_eq!(a.get_deep_value(), b.get_deep_value());
    println!("P: Y, Z, X; Q: Y, Z, X. Stable relation IDs and shared node content preserved.");
    Ok(())
}
