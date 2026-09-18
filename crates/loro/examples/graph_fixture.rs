//! Emit real native Graph formats for the other in-repository reader tests.
use loro::{ExportMode, LoroDoc, ToJson, VersionVector};
use std::{fs, path::PathBuf};

fn main() -> Result<(), Box<dyn std::error::Error>> {
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
    graph.node_meta(a)?.insert("name", "native graph fixture")?;
    graph.create_edge(a, b)?;
    graph.create_edge(b, a)?;
    graph.create_edge(a, a)?;
    doc.commit();
    let base = doc.state_frontiers();
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
    fs::write(
        output.join("graph-expected.json"),
        serde_json::to_vec_pretty(&doc.get_deep_value().to_json_value())?,
    )?;
    println!("{}", output.display());
    Ok(())
}
