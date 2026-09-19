use super::{copy_diff_fixture::*, oracle::Kind, support::*};
use loro::ContainerTrait;

const EXPECTED: [&str; 5] = [
    "g1-prefix",
    "g1-copy",
    "g1-suffix",
    "g2-copy",
    "g2-existing",
];

#[test]
fn copy_diff_keeps_later_original_key_groups_after_the_entire_adjusted_suffix() {
    let case = groups(false);
    let [prefix, _, suffix, _, existing2] = case.ids;
    let source = case.source.get_graph("g");
    let target = case.target.get_graph("g");
    assert_eq!(names(&source, case.parent), EXPECTED);
    let k1 = target.edge_record(prefix).unwrap().position;
    let k2 = target.edge_record(existing2).unwrap().position;
    let metadata: Vec<_> = [prefix, suffix, existing2]
        .into_iter()
        .map(|id| {
            let meta = target.edge_meta(id).unwrap();
            (id, meta.id(), meta.get_deep_value())
        })
        .collect();

    case.target
        .apply_diff(
            case.source
                .diff(&case.v0, &case.source.state_frontiers())
                .unwrap(),
        )
        .unwrap();
    case.target.commit();
    assert_eq!(
        names(&target, case.parent),
        EXPECTED,
        "G2 must follow every G1 member after G1's physical keys change"
    );
    let c1 = target.edge_record(find(&target, "g1-copy")).unwrap();
    let adjusted = target.edge_record(suffix).unwrap();
    let c2 = target.edge_record(find(&target, "g2-copy")).unwrap();
    assert!(k1 < c1.position && c1.position < adjusted.position);
    assert!(adjusted.position < c2.position && c2.position < k2);
    assert_eq!(target.edge_record(prefix).unwrap().position, k1);
    assert_eq!(target.edge_record(existing2).unwrap().position, k2);
    for (id, meta_id, value) in metadata {
        assert!(target.get_edge(id).is_some());
        assert_eq!(target.edge_meta(id).unwrap().id(), meta_id);
        assert_eq!(target.edge_meta(id).unwrap().get_deep_value(), value);
    }
    check(&case.target, "g", &history(&case.target));
}

#[test]
fn copy_diff_groups_use_final_lifecycle_including_net_delete_and_restore() {
    for action in ["delete-edge", "delete-parent", "restore-edge"] {
        let case = groups(action == "restore-edge");
        let source = case.source.get_graph("g");
        let target = case.target.get_graph("g");
        let [prefix, _, suffix, copied2, existing2] = case.ids;
        let before_suffix = target.edge_record(suffix).unwrap();
        let before_existing2 = target.edge_record(existing2).unwrap();
        let source_copied2 = source.edge_record(copied2).unwrap().position;
        let before = case.target.oplog_vv();
        match action {
            "delete-edge" => {
                source.delete_edge(suffix).unwrap();
                source.delete_edge(copied2).unwrap();
            }
            "delete-parent" => source.delete_node(case.parent).unwrap(),
            "restore-edge" => source.restore_edge(suffix).unwrap(),
            _ => unreachable!(),
        }
        case.source.commit();
        case.target
            .apply_diff(
                case.source
                    .diff(&case.v0, &case.source.state_frontiers())
                    .unwrap(),
            )
            .unwrap();
        case.target.commit();
        let tail = super::oracle::History::from_json(&json_between(
            &case.target,
            &before,
            &case.target.oplog_vv(),
        ));
        match action {
            "restore-edge" => {
                assert_eq!(names(&source, case.parent), EXPECTED);
                assert_eq!(names(&target, case.parent), EXPECTED);
                assert!(target.get_edge(suffix).is_some());
                assert!(target.edge_record(suffix).unwrap().position > before_suffix.position);
            }
            "delete-edge" => {
                assert_eq!(
                    names(&target, case.parent),
                    ["g1-prefix", "g1-copy", "g2-existing"]
                );
                let hidden_copy = target.edge_record(find(&target, "g2-copy")).unwrap();
                assert!(!hidden_copy.alive && !hidden_copy.visible);
                assert_eq!(hidden_copy.position, source_copied2);
                let hidden_existing = target.edge_record(suffix).unwrap();
                assert!(!hidden_existing.alive && !hidden_existing.visible);
                assert_eq!(hidden_existing.position, before_suffix.position);
                assert_eq!(hidden_existing.last_order, before_suffix.last_order);
                assert!(!tail
                    .0
                    .values()
                    .any(|fact| matches!(fact.kind, Kind::Order { id, .. } if id == eid(suffix))));
            }
            "delete-parent" => {
                assert!(order(&target, case.parent).is_empty());
                assert!(!target.node_record(case.parent).unwrap().alive);
                for id in [prefix, suffix, existing2] {
                    let edge = target.edge_record(id).unwrap();
                    assert!(edge.alive && !edge.visible);
                }
                assert_eq!(
                    target.edge_record(suffix).unwrap().position,
                    before_suffix.position
                );
                assert_eq!(
                    target.edge_record(existing2).unwrap().position,
                    before_existing2.position
                );
                assert!(!tail
                    .0
                    .values()
                    .any(|fact| matches!(fact.kind, Kind::Order { .. })));
            }
            _ => unreachable!(),
        }
        check(&case.target, "g", &history(&case.target));
    }
}
