use super::{
    copy_diff_fixture::*,
    oracle::{History, Kind},
    support::*,
};
use loro::{ContainerTrait, ExportMode, GraphOrderTarget as At};

#[test]
fn copy_diff_replaces_existing_order_plans_when_collision_suffix_also_changes_them() {
    let seed = doc(1);
    let parent = seed.get_graph("g").create_node().unwrap();
    let upper = named(&seed.get_graph("g"), parent, "upper");
    seed.commit();
    let left_base = copy(&seed, 2);
    let left = named(&left_base.get_graph("g"), parent, "left");
    left_base.commit();
    let base = copy(&left_base, 9);
    let suffix = named(&base.get_graph("g"), parent, "suffix");
    base.commit();
    let v0 = base.state_frontiers();
    let source = copy(&base, 4);
    let local = source.get_graph("g");
    let copied = local.create_edge_at(parent, parent, At::Start).unwrap();
    local
        .edge_meta(copied)
        .unwrap()
        .insert("name", "copy")
        .unwrap();
    source.commit();
    let left_move = copy(&base, 2);
    let right_move = copy(&base, 9);
    left_move
        .get_graph("g")
        .reorder_edge(left, At::Start)
        .unwrap();
    right_move
        .get_graph("g")
        .reorder_edge(suffix, At::Start)
        .unwrap();
    left_move.commit();
    right_move.commit();
    for branch in [&left_move, &right_move] {
        source
            .import(&branch.export(ExportMode::all_updates()).unwrap())
            .unwrap();
    }
    assert_eq!(names(&local, parent), ["left", "copy", "suffix", "upper"]);
    let k1 = local.edge_record(copied).unwrap().position;
    assert_eq!(local.edge_record(left).unwrap().position, k1);
    assert_eq!(local.edge_record(suffix).unwrap().position, k1);
    let target = copy(&base, 20);
    let g = target.get_graph("g");
    assert_ne!(g.edge_record(left).unwrap().position, k1);
    assert_ne!(g.edge_record(suffix).unwrap().position, k1);
    let suffix_meta = g.edge_meta(suffix).unwrap();
    let meta_id = suffix_meta.id();
    let meta_value = suffix_meta.get_deep_value();
    let before = target.oplog_vv();
    target
        .apply_diff(source.diff(&v0, &source.state_frontiers()).unwrap())
        .unwrap();
    target.commit();

    assert_eq!(names(&g, parent), ["left", "copy", "suffix", "upper"]);
    let copy_position = g.edge_record(find(&g, "copy")).unwrap().position;
    let suffix_position = g.edge_record(suffix).unwrap().position;
    assert_eq!(g.edge_record(left).unwrap().position, k1);
    assert!(k1 < copy_position && copy_position < suffix_position);
    assert!(suffix_position < g.edge_record(upper).unwrap().position);
    let tail = History::from_json(&json_between(&target, &before, &target.oplog_vv()));
    for id in [left, suffix] {
        let writes = tail
            .0
            .values()
            .filter(
                |fact| matches!(fact.kind, Kind::Order { id: written, .. } if written == eid(id)),
            )
            .count();
        assert_eq!(
            writes, 1,
            "replace the original planned Set; do not emit it plus an auxiliary Set"
        );
    }
    assert_eq!(g.edge_meta(suffix).unwrap().id(), meta_id);
    assert_eq!(g.edge_meta(suffix).unwrap().get_deep_value(), meta_value);
    check(&target, "g", &history(&target));
}
