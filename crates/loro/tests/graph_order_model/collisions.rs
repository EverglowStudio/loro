use super::{oracle::Kind, support::*};
use loro::{GraphOrderTarget as At, VersionVector};
use std::collections::BTreeSet;

#[test]
fn same_peer_collisions_are_stable_and_queries_do_not_repair() {
    let (d, n, e) = fixture(&["80", "80", "80", "80"]);
    let g = d.get_graph("g");
    let before = json(&d);
    let vv = d.oplog_vv();
    let frontiers = d.state_frontiers();
    for jitter in [0, 8, 0] {
        g.configure_order_jitter(jitter);
        assert_eq!(order(&g, n), e);
        check(&d, "g", &history(&d));
        assert_eq!(json(&d), before);
        assert_eq!(d.get_pending_txn_len(), 0);
        assert_eq!(d.oplog_vv(), vv);
        assert_eq!(d.state_frontiers(), frontiers);
    }
    let remote = copy(&d, 2);
    remote.get_graph("g").configure_order_jitter(16);
    sync(&d, &remote);
    assert_eq!(order(&remote.get_graph("g"), n), e);
    assert_eq!(remote.get_pending_txn_len(), 0);
    assert_eq!(remote.oplog_vv(), vv);
}

#[test]
fn insertion_opens_only_the_required_visible_equal_key_suffix() {
    // All-equal, equal prefix, middle, and suffix, including both outer gaps.
    for (keys, index) in [
        (vec!["80", "80", "80", "80"], 2),
        (vec!["80", "80", "80", "c080"], 1),
        (vec!["4080", "80", "80", "c080"], 2),
        (vec!["4080", "80", "80", "80"], 2),
        (vec!["80", "80", "80"], 0),
        (vec!["80", "80", "80"], 3),
    ] {
        let (d, n, edges) = fixture(&keys);
        let g = d.get_graph("g");
        let old_positions: Vec<_> = edges
            .iter()
            .map(|e| g.edge_record(*e).unwrap().position)
            .collect();
        let before = d.oplog_vv();
        let at = edges.get(index).copied().map_or(At::End, At::Before);
        let inserted = g.create_edge_at(n, n, at).unwrap();
        let mut intended = edges.clone();
        intended.insert(index, inserted);
        assert_eq!(order(&g, n), intended, "keys={keys:?}, index={index}");
        d.commit();
        let edits = super::oracle::History::from_json(&json_between(&d, &before, &d.oplog_vv()));
        let adjusted: BTreeSet<_> = edits
            .0
            .values()
            .filter_map(|f| match f.kind {
                Kind::Order { id, .. } => Some(edge(id)),
                _ => None,
            })
            .collect();
        let mut expected_adjusted = BTreeSet::new();
        if index > 0 && index < edges.len() && keys[index - 1] == keys[index] {
            for j in index..edges.len() {
                if keys[j] != keys[index] {
                    break;
                }
                expected_adjusted.insert(edges[j]);
            }
        }
        assert_eq!(adjusted, expected_adjusted, "unnecessary collision rewrite");
        for (i, e) in edges.iter().enumerate() {
            if !expected_adjusted.contains(e) {
                assert_eq!(g.edge_record(*e).unwrap().position, old_positions[i]);
            }
        }
        let rows = g.ordered_out_edges(n).unwrap();
        if index > 0 {
            assert!(rows[index - 1].position < rows[index].position);
        }
        if index + 1 < rows.len() {
            assert!(rows[index].position < rows[index + 1].position);
        }
        check(&d, "g", &history(&d));
    }
}

#[test]
fn moving_a_member_of_a_collision_run_preserves_other_relative_order() {
    for (moving, anchor) in [(3, 1), (0, 2), (1, 3)] {
        let (d, n, e) = fixture(&["80", "80", "80", "80", "c080"]);
        let g = d.get_graph("g");
        let mut intended = e.clone();
        intended.remove(moving);
        let at = intended.iter().position(|id| *id == e[anchor]).unwrap();
        intended.insert(at, e[moving]);
        assert!(
            g.reorder_edge(e[moving], At::Before(e[anchor]))
                .unwrap()
                .changed
        );
        assert_eq!(order(&g, n), intended);
        d.commit();
        check(&d, "g", &history(&d));
    }
}

#[test]
fn hidden_edges_are_not_rewritten_to_make_a_visible_collision_gap() {
    let (d, n, e) = fixture(&["80", "80", "80", "80", "c080"]);
    let g = d.get_graph("g");
    g.delete_edge(e[2]).unwrap();
    d.commit();
    let hidden = g.edge_record(e[2]).unwrap();
    let old = d.oplog_vv();
    let inserted = g.create_edge_at(n, n, At::Before(e[1])).unwrap();
    assert_eq!(order(&g, n), vec![e[0], inserted, e[1], e[3], e[4]]);
    d.commit();
    let next = g.edge_record(e[2]).unwrap();
    assert_eq!(next.position, hidden.position);
    assert_eq!(next.last_order, hidden.last_order);
    let facts = super::oracle::History::from_json(&json_between(&d, &old, &d.oplog_vv()));
    assert!(!facts
        .0
        .values()
        .any(|f| matches!(f.kind, Kind::Order { id, .. } if id == eid(e[2]))));
    g.restore_edge(e[2]).unwrap();
    assert_eq!(order(&g, n), vec![e[0], e[2], inserted, e[1], e[3], e[4]]);
    d.commit();
    check(&d, "g", &history(&d));
    // Also exercise a fresh edit after replaying the collision batch.
    let remote = doc(19);
    remote
        .import_json_updates(
            d.export_json_updates_without_peer_compression(
                &VersionVector::default(),
                &d.oplog_vv(),
            ),
        )
        .unwrap();
    remote
        .get_graph("g")
        .reorder_edge(e[4], At::After(e[2]))
        .unwrap();
    assert_eq!(
        order(&remote.get_graph("g"), n),
        vec![e[0], e[2], e[4], inserted, e[1], e[3]]
    );
}
