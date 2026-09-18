use loro::{
    graph::{
        analyze_cycles, apply_repair, plan_break_cycles, GraphRepairError, GraphScope, RepairPolicy,
    },
    ContainerTrait, ExportMode, LoroDoc,
};

#[test]
fn helper_reads_are_pure_and_pending_edits_are_not_committed() {
    let doc = LoroDoc::new();
    doc.set_peer_id(1).unwrap();
    let graph = doc.get_graph("graph");
    let a = graph.create_node().unwrap();
    let b = graph.create_node().unwrap();
    graph.create_edge(a, b).unwrap();
    graph.create_edge(b, a).unwrap();
    let before = doc.oplog_vv();
    assert_eq!(
        graph.snapshot().unwrap_err(),
        GraphRepairError::UncommittedChanges
    );
    assert_eq!(before, doc.oplog_vv());
    doc.commit();
    let before = doc.oplog_vv();
    let frontiers = doc.state_frontiers();
    let snapshot = graph.snapshot().unwrap();
    let report = analyze_cycles(&snapshot, &GraphScope::AllVisible).unwrap();
    let plan = plan_break_cycles(
        &snapshot,
        &GraphScope::AllVisible,
        RepairPolicy::AscendingNodeIdV1,
    )
    .unwrap();
    assert!(!report.is_acyclic());
    assert!(!plan.is_empty());
    assert_eq!(before, doc.oplog_vv());
    assert_eq!(frontiers, doc.state_frontiers());
    assert_eq!(graph.edge_count(), 2);
}

#[test]
fn repair_scope_staleness_empty_and_repeat_are_explicit() {
    let doc = LoroDoc::new();
    doc.set_peer_id(1).unwrap();
    let graph = doc.get_graph("graph");
    let a = graph.create_node().unwrap();
    let b = graph.create_node().unwrap();
    let f = graph.create_edge(a, b).unwrap();
    let r = graph.create_edge(b, a).unwrap();
    let outside = graph.create_edge(a, a).unwrap();
    doc.commit();
    let snapshot = graph.snapshot().unwrap();
    let empty = plan_break_cycles(
        &snapshot,
        &GraphScope::Selected(vec![]),
        RepairPolicy::AscendingNodeIdV1,
    )
    .unwrap();
    let vv = doc.oplog_vv();
    assert_eq!(apply_repair(&graph.to_handler(), &empty).unwrap(), 0);
    assert_eq!(vv, doc.oplog_vv());
    let scope = GraphScope::Selected(vec![f, r]);
    let plan = plan_break_cycles(&snapshot, &scope, RepairPolicy::AscendingNodeIdV1).unwrap();
    assert_eq!(apply_repair(&graph.to_handler(), &plan).unwrap(), 1);
    assert!(graph.edge_record(outside).unwrap().visible);
    assert_eq!(
        apply_repair(&graph.to_handler(), &plan).unwrap_err(),
        GraphRepairError::UncommittedChanges
    );
    doc.commit();
    assert_eq!(
        apply_repair(&graph.to_handler(), &plan).unwrap_err(),
        GraphRepairError::StalePlan
    );
    // A fresh selected scope includes only remaining visible selected edges.
    let report =
        analyze_cycles(&graph.snapshot().unwrap(), &GraphScope::Selected(vec![f])).unwrap();
    assert!(report.is_acyclic());
    assert!(
        !analyze_cycles(&graph.snapshot().unwrap(), &GraphScope::AllVisible)
            .unwrap()
            .is_acyclic()
    );
}

#[test]
fn repair_checks_actual_docstate_and_container_identity() {
    let doc = LoroDoc::new();
    doc.set_peer_id(1).unwrap();
    let graph = doc.get_graph("graph");
    let a = graph.create_node().unwrap();
    graph.create_edge(a, a).unwrap();
    doc.commit();
    let old = doc.state_frontiers();
    let plan = plan_break_cycles(
        &graph.snapshot().unwrap(),
        &GraphScope::AllVisible,
        RepairPolicy::AscendingNodeIdV1,
    )
    .unwrap();
    let other = doc.get_graph("other");
    assert_eq!(
        apply_repair(&other.to_handler(), &plan).unwrap_err(),
        GraphRepairError::WrongGraph
    );
    graph.create_node().unwrap();
    doc.commit();
    assert_eq!(
        apply_repair(&graph.to_handler(), &plan).unwrap_err(),
        GraphRepairError::StalePlan
    );
    doc.checkout(&old).unwrap();
    assert_eq!(graph.snapshot().unwrap().version(), &old);
    assert!(apply_repair(&graph.to_handler(), &plan).is_err());
    doc.attach();
}

#[test]
fn concurrent_repairs_merge_as_concrete_deletions_and_later_restore_can_cycle() {
    let a = LoroDoc::new();
    a.set_peer_id(1).unwrap();
    let ga = a.get_graph("g");
    let x = ga.create_node().unwrap();
    let y = ga.create_node().unwrap();
    let f = ga.create_edge(x, y).unwrap();
    let r = ga.create_edge(y, x).unwrap();
    a.commit();
    let b = LoroDoc::new();
    b.set_peer_id(2).unwrap();
    b.import(&a.export(ExportMode::snapshot()).unwrap())
        .unwrap();
    let gb = b.get_graph("g");
    let loop_b = gb.create_edge(y, y).unwrap();
    b.commit();
    let pa = plan_break_cycles(
        &ga.snapshot().unwrap(),
        &GraphScope::AllVisible,
        RepairPolicy::AscendingNodeIdV1,
    )
    .unwrap();
    let pb = plan_break_cycles(
        &gb.snapshot().unwrap(),
        &GraphScope::AllVisible,
        RepairPolicy::AscendingNodeIdV1,
    )
    .unwrap();
    apply_repair(&ga.to_handler(), &pa).unwrap();
    apply_repair(&gb.to_handler(), &pb).unwrap();
    a.commit();
    b.commit();
    let ua = a.export(ExportMode::all_updates()).unwrap();
    let ub = b.export(ExportMode::all_updates()).unwrap();
    a.import(&ub).unwrap();
    b.import(&ua).unwrap();
    assert_eq!(a.get_deep_value(), b.get_deep_value());
    assert!(ga.edge_record(f).unwrap().visible);
    assert!(!ga.edge_record(r).unwrap().visible);
    assert!(!ga.edge_record(loop_b).unwrap().visible);
    assert!(
        analyze_cycles(&ga.snapshot().unwrap(), &GraphScope::AllVisible)
            .unwrap()
            .is_acyclic()
    );
    ga.restore_edge(r).unwrap();
    a.commit();
    assert!(
        !analyze_cycles(&ga.snapshot().unwrap(), &GraphScope::AllVisible)
            .unwrap()
            .is_acyclic()
    );
}

#[test]
fn concurrent_apply_checks_and_writes_share_the_same_local_transaction_lock() {
    use std::sync::{Arc, Barrier};
    let doc = LoroDoc::new();
    doc.set_peer_id(1).unwrap();
    let graph = doc.get_graph("graph");
    let a = graph.create_node().unwrap();
    graph.create_edge(a, a).unwrap();
    doc.commit();
    let plan = plan_break_cycles(
        &graph.snapshot().unwrap(),
        &GraphScope::AllVisible,
        RepairPolicy::AscendingNodeIdV1,
    )
    .unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let workers: Vec<_> = (0..2)
        .map(|_| {
            let graph = graph.clone();
            let plan = plan.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                apply_repair(&graph.to_handler(), &plan)
            })
        })
        .collect();
    let outcomes: Vec<_> = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect();
    assert_eq!(
        outcomes.iter().filter(|result| **result == Ok(1)).count(),
        1
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|result| **result == Err(GraphRepairError::UncommittedChanges))
            .count(),
        1
    );
    doc.commit();
    assert_eq!(graph.edge_count(), 0);
    assert_eq!(graph.edge_records()[0].delete_tags.len(), 1);
}
