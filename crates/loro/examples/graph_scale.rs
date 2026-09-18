//! Reproducible native graph scale fixture. Build first; time only the executable.
use loro::{
    graph::{analyze_cycles, plan_break_cycles, GraphScope, RepairPolicy},
    ExportMode, LoroDoc, LoroValue, VersionVector,
};
use serde_json::json;
use std::{collections::BTreeMap, time::Instant};

fn measure<T>(phases: &mut BTreeMap<String, f64>, name: &str, f: impl FnOnce() -> T) -> T {
    let start = Instant::now();
    let result = f();
    phases.insert(name.to_owned(), start.elapsed().as_secs_f64() * 1000.0);
    result
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    let n: usize = args.get(1).map(String::as_str).unwrap_or("1000").parse()?;
    let shape = args.get(2).map(String::as_str).unwrap_or("sparse");
    if n < 2 {
        return Err("node count must be at least two".into());
    }
    if ![
        "sparse",
        "hub",
        "chain",
        "scc",
        "parallel",
        "delete-history",
    ]
    .contains(&shape)
    {
        return Err("shape must be sparse, hub, chain, scc, parallel or delete-history".into());
    }
    let doc = LoroDoc::new();
    doc.set_peer_id(11)?;
    let graph = doc.get_graph("graph");
    let mut phases = BTreeMap::new();
    let nodes = measure(&mut phases, "create_nodes", || {
        (0..n)
            .map(|_| graph.create_node().unwrap())
            .collect::<Vec<_>>()
    });
    let edges = measure(&mut phases, "create_edges", || {
        let pairs: Vec<_> = match shape {
            "hub" => (1..n).flat_map(|i| [(0, i), (i, 0)]).collect(),
            "parallel" => (1..n)
                .flat_map(|i| [(i - 1, i), (i - 1, i), (i, i - 1)])
                .collect(),
            "scc" => (0..n)
                .flat_map(|i| [(i, (i + 1) % n), (i, (i + 2) % n)])
                .collect(),
            "chain" => (1..n).map(|i| (i - 1, i)).collect(),
            _ => (1..n).flat_map(|i| [(i - 1, i), (i / 2, i)]).collect(),
        };
        pairs
            .into_iter()
            .map(|(s, t)| graph.create_edge(nodes[s], nodes[t]).unwrap())
            .collect::<Vec<_>>()
    });
    measure(&mut phases, "node_and_edge_properties", || {
        // A bounded sample isolates metadata access from topology scale.
        for &node in nodes.iter().step_by((n / 100).max(1)) {
            graph
                .node_meta(node)
                .unwrap()
                .insert("label", LoroValue::from("sample"))
                .unwrap();
        }
        for &edge in edges.iter().step_by((edges.len() / 100).max(1)) {
            graph
                .edge_meta(edge)
                .unwrap()
                .insert("relation", LoroValue::from("sample"))
                .unwrap();
        }
    });
    doc.commit();
    let before = doc.state_frontiers();
    let base_version = doc.oplog_vv();
    let base_updates = doc.export(ExportMode::all_updates())?;
    let history_rounds = if shape == "delete-history" { 8 } else { 1 };
    measure(&mut phases, "delete_restore_nodes", || {
        for _ in 0..history_rounds {
            for &node in nodes.iter().take((n / 10).max(1)) {
                graph.delete_node(node).unwrap();
            }
            for &node in nodes.iter().take((n / 10).max(1)) {
                graph.restore_node(node).unwrap();
            }
        }
    });
    measure(&mut phases, "delete_restore_edges", || {
        for &edge in edges.iter().take((edges.len() / 10).max(1)) {
            graph.delete_edge(edge).unwrap();
        }
        for &edge in edges.iter().take((edges.len() / 10).max(1)) {
            graph.restore_edge(edge).unwrap();
        }
    });
    doc.commit();
    measure(&mut phases, "record_lookup", || {
        for &node in &nodes {
            assert!(graph.node_record(node).unwrap().visible);
        }
        for &edge in &edges {
            assert!(graph.edge_record(edge).unwrap().visible);
        }
    });
    measure(&mut phases, "adjacency", || {
        let mut out = 0;
        let mut input = 0;
        for &node in &nodes {
            out += graph.outgoing_edges(node).unwrap().len();
            input += graph.incoming_edges(node).unwrap().len();
        }
        assert_eq!(out, edges.len());
        assert_eq!(input, edges.len());
    });
    let updates = measure(&mut phases, "updates_encode", || {
        doc.export(ExportMode::updates(&VersionVector::default()))
            .unwrap()
    });
    // Measure the causal validation/application of lifecycle-only updates
    // separately from creating the topology. Drop this receiver before the
    // ordinary full import to keep the maximum live document count at three.
    let lifecycle_updates = doc.export(ExportMode::updates(&base_version))?;
    {
        let staged = LoroDoc::new();
        measure(&mut phases, "topology_only_import", || {
            staged.import(&base_updates).unwrap()
        });
        measure(&mut phases, "lifecycle_only_import", || {
            staged.import(&lifecycle_updates).unwrap()
        });
        assert_eq!(doc.get_deep_value(), staged.get_deep_value());
    }
    let peer = LoroDoc::new();
    peer.set_peer_id(12)?;
    measure(&mut phases, "sync_import", || {
        peer.import(&updates).unwrap()
    });
    assert_eq!(doc.get_deep_value(), peer.get_deep_value());
    let snapshot = measure(&mut phases, "snapshot_encode", || {
        doc.export(ExportMode::snapshot()).unwrap()
    });
    let reopened = LoroDoc::new();
    measure(&mut phases, "snapshot_decode", || {
        reopened.import(&snapshot).unwrap();
        // Force lazy state materialization so decode is not only wrapper loading.
        assert_eq!(reopened.get_graph("graph").node_count(), n);
        assert_eq!(reopened.get_graph("graph").edge_count(), edges.len());
    });
    measure(&mut phases, "checkout_roundtrip", || {
        doc.checkout(&before).unwrap();
        doc.attach();
    });
    assert_eq!(doc.get_deep_value(), peer.get_deep_value());
    let graph_snapshot = measure(&mut phases, "helper_snapshot", || graph.snapshot().unwrap());
    let report = measure(&mut phases, "cycle_analysis", || {
        analyze_cycles(&graph_snapshot, &GraphScope::AllVisible).unwrap()
    });
    let plan = measure(&mut phases, "repair_plan", || {
        plan_break_cycles(
            &graph_snapshot,
            &GraphScope::AllVisible,
            RepairPolicy::AscendingNodeIdV1,
        )
        .unwrap()
    });
    let total_ops: i64 = doc
        .oplog_vv()
        .iter()
        .map(|(_, counter)| i64::from(*counter))
        .sum();
    let base_ops: i64 = base_version
        .iter()
        .map(|(_, counter)| i64::from(*counter))
        .sum();
    println!(
        "{}",
        json!({
            "schema":1,"shape":shape,"nodes":n,"edges":edges.len(),"replicas":2,
            "history_rounds":history_rounds,"operations":total_ops,"lifecycle_operations":total_ops-base_ops,
            "updates_bytes":updates.len(),"lifecycle_updates_bytes":lifecycle_updates.len(),"snapshot_bytes":snapshot.len(),"cyclic_components":report.cyclic_components.len(),
            "planned_deletions":plan.deletions().len(),"phases_ms":phases,
            "assertions":{"replicas_equal":true,"lifecycle_import_equal":true,"record_visibility":true,"snapshot_counts":true,"checkout_equal":true,"adjacency_counts":true},
            "notes":"native process; bounded property sample; snapshot decode forces graph load; no repair operations applied"
        })
    );
    Ok(())
}
