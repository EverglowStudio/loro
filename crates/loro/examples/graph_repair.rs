//! Inspect a concrete plan, then explicitly apply its ordinary edge deletions.
use loro::{
    graph::{analyze_cycles, apply_repair, plan_break_cycles, GraphScope, RepairPolicy},
    ContainerTrait, LoroDoc,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let doc = LoroDoc::new();
    doc.set_peer_id(1)?;
    let graph = doc.get_graph("relationships");
    let a = graph.create_node()?;
    let b = graph.create_node()?;
    let c = graph.create_node()?;
    graph.create_edge(a, b)?;
    graph.create_edge(b, c)?;
    graph.create_edge(c, a)?;
    graph.create_edge(a, a)?;
    doc.commit();
    let version = doc.oplog_vv();
    let snapshot = graph.snapshot()?;
    let report = analyze_cycles(&snapshot, &GraphScope::AllVisible)?;
    let plan = plan_break_cycles(
        &snapshot,
        &GraphScope::AllVisible,
        RepairPolicy::AscendingNodeIdV1,
    )?;
    println!("Cyclic components: {:?}", report.cyclic_components);
    println!("Proposed edge deletions: {:?}", plan.deletions());
    assert_eq!(version, doc.oplog_vv());
    assert_eq!(graph.edge_count(), 4);
    // The application makes this decision. No listener or background repair runs.
    let applied = apply_repair(&graph.to_handler(), &plan)?;
    doc.commit();
    assert!(analyze_cycles(&graph.snapshot()?, &GraphScope::AllVisible)?.is_acyclic());
    println!("Applied {applied} normal edge deletions");
    // A later edit can introduce a cycle again; the core remains a general graph.
    graph.create_edge(c, a)?;
    doc.commit();
    assert!(!analyze_cycles(&graph.snapshot()?, &GraphScope::AllVisible)?.is_acyclic());
    Ok(())
}
