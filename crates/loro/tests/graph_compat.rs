//! Existing baseline bytes stay fixed while the native container table grows.
use loro::{ContainerID, ContainerType, ExportMode, LoroDoc, ToJson};

#[test]
fn graph_tag_does_not_renumber_existing_container_types() {
    let expected = [
        (ContainerType::Map, 0),
        (ContainerType::List, 1),
        (ContainerType::Text, 2),
        (ContainerType::Tree, 3),
        (ContainerType::MovableList, 4),
        (ContainerType::Graph, 6),
    ];
    for (kind, tag) in expected {
        assert_eq!(kind.to_u8(), tag);
        assert_eq!(ContainerType::try_from_u8(tag).unwrap(), kind);
        let id = ContainerID::new_root("fixture", kind);
        assert_eq!(ContainerID::try_from(id.to_string().as_str()).unwrap(), id);
        assert_eq!(ContainerID::try_from_bytes(&id.to_bytes()).unwrap(), id);
    }
    #[cfg(feature = "counter")]
    assert_eq!(ContainerType::Counter.to_u8(), 5);
}

#[test]
fn baseline_binary_snapshot_and_updates_preserve_all_old_types() {
    let snapshot = include_bytes!("../../../loro-js/tests/fixtures/rust/runtime-snapshot.ts.blob");
    let updates = include_bytes!("../../../loro-js/tests/fixtures/rust/runtime-updates.ts.blob");
    let expected: serde_json::Value = serde_json::from_str(include_str!(
        "../../../loro-js/tests/fixtures/rust/runtime.expected.json"
    ))
    .unwrap();
    for bytes in [snapshot.as_slice(), updates.as_slice()] {
        let doc = LoroDoc::new();
        doc.import(bytes).unwrap();
        assert_eq!(doc.get_deep_value().to_json_value(), expected);
        let copy = LoroDoc::new();
        copy.import(&doc.export(ExportMode::snapshot()).unwrap())
            .unwrap();
        assert_eq!(copy.get_deep_value().to_json_value(), expected);
        let update_copy = LoroDoc::new();
        update_copy
            .import(&doc.export(ExportMode::all_updates()).unwrap())
            .unwrap();
        assert_eq!(update_copy.get_deep_value().to_json_value(), expected);
    }
}
