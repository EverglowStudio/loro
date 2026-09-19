//! Emit real native Graph formats for the other in-repository reader tests.
use loro::{
    ExportMode, GraphEdgeId, GraphNodeId, GraphOrderTarget, LoroDoc, ToJson, VersionVector,
};
use std::{
    fs,
    path::{Path, PathBuf},
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::args().nth(1).as_deref() == Some("--verify-wasm") {
        let fixtures = PathBuf::from(
            std::env::args()
                .nth(2)
                .ok_or("provide native fixture directory")?,
        );
        let continued = PathBuf::from(
            std::env::args()
                .nth(3)
                .ok_or("provide WASM continued-snapshot directory")?,
        );
        return verify_wasm(&fixtures, &continued);
    }
    let output = PathBuf::from(
        std::env::args()
            .nth(1)
            .ok_or("provide an output directory")?,
    );
    fs::create_dir_all(&output)?;
    let doc = LoroDoc::new();
    doc.set_peer_id(18_446_744_073_709_551_600)?;
    let graph = doc.get_graph("graph");
    let a = graph.create_node()?;
    let b = graph.create_node()?;
    let c = graph.create_node()?;
    graph.node_meta(a)?.insert("name", "native graph fixture")?;
    let ab = graph.create_edge(a, b)?;
    graph.edge_meta(ab)?.insert("role", "parallel relation")?;
    graph.create_edge(b, a)?;
    graph.create_edge(a, a)?;
    let ac = graph.create_edge(a, c)?;
    let bc = graph.create_edge(b, c)?;
    doc.commit();
    let base = doc.state_frontiers();

    // Two independent inserts deliberately allocate the same key with jitter=0.
    let left = doc.fork();
    left.set_peer_id(20)?;
    let x = left
        .get_graph("graph")
        .create_edge_at(a, b, GraphOrderTarget::Before(ac))?;
    left.commit();
    let right = doc.fork();
    right.set_peer_id(30)?;
    let y = right
        .get_graph("graph")
        .create_edge_at(a, b, GraphOrderTarget::Before(ac))?;
    right.commit();
    assert_eq!(
        left.get_graph("graph").edge_record(x).unwrap().position,
        right.get_graph("graph").edge_record(y).unwrap().position
    );
    doc.import(&right.export(ExportMode::all_updates())?)?;
    doc.import(&left.export(ExportMode::all_updates())?)?;
    let collision = graph.reorder_edge(ab, GraphOrderTarget::Before(y))?;
    assert_eq!(collision.auxiliary_updates, 1);
    doc.commit();

    let remote = doc.fork();
    remote.set_peer_id(40)?;
    remote
        .get_graph("graph")
        .reorder_edge(y, GraphOrderTarget::Start)?;
    remote
        .get_graph("graph")
        .edge_meta(y)?
        .insert("note", "remote metadata")?;
    remote.commit();
    graph.reorder_edge(y, GraphOrderTarget::End)?;
    doc.commit();
    let remote_update = remote.export(ExportMode::all_updates())?;
    doc.import(&remote_update)?;
    doc.import(&remote_update)?;

    graph.delete_node(a)?;
    doc.commit();
    graph.restore_node(a)?;
    doc.commit();
    for (name, mode) in [
        ("graph-snapshot.bin", ExportMode::snapshot()),
        ("graph-updates.bin", ExportMode::all_updates()),
        ("graph-shallow.bin", ExportMode::shallow_snapshot(&base)),
        ("graph-state-only.bin", ExportMode::StateOnly(None)),
    ] {
        let bytes = doc.export(mode)?;
        fs::write(output.join(name), bytes)?;
    }
    fs::write(
        output.join("graph-updates.json"),
        serde_json::to_vec_pretty(&doc.export_json_updates_without_peer_compression(
            &VersionVector::default(),
            &doc.oplog_vv(),
        ))?,
    )?;
    let ordered_sources: Vec<_> = [a, b, c]
        .into_iter()
        .map(|source| {
            let edges = graph.ordered_out_edges(source).unwrap();
            serde_json::json!({
                "source": source.to_string(),
                "edges": edges.iter().map(|edge| serde_json::json!({
                    "id": edge.edge_id.to_string(),
                    "target": edge.target.to_string(),
                    "position": edge.position.to_string(),
                })).collect::<Vec<_>>()
            })
        })
        .collect();
    fs::write(
        output.join("graph-manifest.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "schema": "lorograph-order-fixture-v1",
            "container": "graph",
            "binaryFormats": ["graph-snapshot.bin", "graph-updates.bin", "graph-shallow.bin", "graph-state-only.bin"],
            "jsonUpdates": "graph-updates.json",
            "deepValue": "graph-expected.json",
            "orderedSources": ordered_sources,
            "identities": {"a": a.to_string(), "b": b.to_string(), "c": c.to_string(), "ab": ab.to_string(), "ac": ac.to_string(), "bc": bc.to_string(), "x": x.to_string(), "y": y.to_string()},
            "assertions": {"sameKeyConcurrentInserts": true, "collisionAuxiliaryUpdates": collision.auxiliary_updates, "duplicateImport": true, "deleteRestore": true}
        }))?,
    )?;
    fs::write(
        output.join("graph-expected.json"),
        serde_json::to_vec_pretty(&doc.get_deep_value().to_json_value())?,
    )?;
    println!("{}", output.display());
    Ok(())
}

/// Read real JS-produced snapshots back through Rust, then merge a native edit.
fn verify_wasm(fixtures: &Path, continued: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(fixtures.join("graph-manifest.json"))?)?;
    let a = GraphNodeId::try_from(manifest["identities"]["a"].as_str().unwrap())?;
    let c = GraphNodeId::try_from(manifest["identities"]["c"].as_str().unwrap())?;
    let seed = LoroDoc::new();
    seed.import(&fs::read(fixtures.join("graph-snapshot.bin"))?)?;
    let originals = seed.get_graph("graph").edge_records();
    let mut paths = fs::read_dir(continued)?
        .map(|e| e.map(|e| e.path()))
        .collect::<Result<Vec<_>, _>>()?;
    paths.sort();
    let mut checked = 0;
    for path in paths.into_iter().filter(|p| {
        p.file_name()
            .unwrap()
            .to_string_lossy()
            .ends_with("-continued.bin")
    }) {
        let doc = LoroDoc::new();
        doc.set_peer_id(200)?;
        doc.import(&fs::read(&path)?)?;
        let graph = doc.get_graph("graph");
        let added: Vec<_> = graph
            .edges()
            .into_iter()
            .filter(|id| !originals.iter().any(|e| e.id == *id))
            .collect();
        assert_eq!(added.len(), 1, "{}", path.display());
        let edge = graph.edge_record(added[0]).unwrap();
        assert_eq!((edge.source, edge.target), (a, c));
        for original in &originals {
            assert_eq!(&graph.edge_record(original.id).unwrap(), original);
            assert_eq!(
                graph.edge_meta(original.id)?.get_deep_value(),
                seed.get_graph("graph")
                    .edge_meta(original.id)?
                    .get_deep_value()
            );
        }
        for domain in manifest["orderedSources"].as_array().unwrap() {
            let source = GraphNodeId::try_from(domain["source"].as_str().unwrap())?;
            let mut expected: Vec<_> = domain["edges"]
                .as_array()
                .unwrap()
                .iter()
                .map(|e| GraphEdgeId::try_from(e["id"].as_str().unwrap()).unwrap())
                .collect();
            if source == a {
                expected.push(added[0]);
            }
            assert_eq!(
                graph
                    .ordered_out_edges(source)?
                    .into_iter()
                    .map(|e| e.edge_id)
                    .collect::<Vec<_>>(),
                expected
            );
            for (rank, id) in expected.iter().enumerate() {
                assert_eq!(graph.index_of_out_edge(*id), Some(rank));
                assert_eq!(graph.out_edge_at(source, rank).unwrap().edge_id, *id);
            }
        }
        let base = doc.oplog_vv();
        let copy = doc.fork();
        graph.reorder_edge(added[0], GraphOrderTarget::Start)?;
        doc.commit();
        copy.import(&doc.export(ExportMode::updates(&base))?)?;
        assert_eq!(copy.get_deep_value(), doc.get_deep_value());
        assert_eq!(
            copy.get_graph("graph").out_edge_at(a, 0).unwrap().edge_id,
            added[0]
        );
        checked += 1;
    }
    assert!(checked > 0, "no WASM continued snapshots found");
    println!(
        "{}",
        serde_json::json!({"verified_wasm_snapshots": checked, "native_continue_sync": true})
    );
    Ok(())
}
