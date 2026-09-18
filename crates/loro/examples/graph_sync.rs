//! A directed multigraph preserves shared identity, parallel edges and cycles.
use loro::{ExportMode, LoroDoc, ToJson};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let alice = LoroDoc::new();
    alice.set_peer_id(1)?;
    let graph = alice.get_graph("relationships");
    let a = graph.create_node()?;
    let b = graph.create_node()?;
    let shared = graph.create_node()?;
    graph
        .node_meta(shared)?
        .insert("name", "one shared object")?;
    graph.create_edge(a, shared)?;
    graph.create_edge(b, shared)?;
    alice.commit();
    let bob = LoroDoc::new();
    bob.set_peer_id(2)?;
    bob.import(&alice.export(ExportMode::snapshot())?)?;
    let other = bob.get_graph("relationships");
    // Offline edits make a cycle when combined. No cycle repair is implicit.
    graph.create_edge(a, b)?;
    let reverse = other.create_edge(b, a)?;
    let parallel = other.create_edge(b, a)?;
    alice.commit();
    bob.commit();
    let left = alice.export(ExportMode::all_updates())?;
    let right = bob.export(ExportMode::all_updates())?;
    alice.import(&right)?;
    bob.import(&left)?;
    assert_eq!(alice.get_deep_value(), bob.get_deep_value());
    assert_eq!(graph.node_count(), 3);
    assert_eq!(graph.edge_count(), 5);
    graph.delete_edge(reverse)?;
    assert!(graph.edge_record(parallel).unwrap().visible);
    graph.delete_node(shared)?;
    assert_eq!(graph.node_count(), 2);
    assert_eq!(graph.edge_count(), 2);
    graph.restore_node(shared)?;
    assert_eq!(graph.node_count(), 3);
    assert_eq!(graph.edge_count(), 4);
    println!("{}", alice.get_deep_value().to_json_value());
    Ok(())
}
