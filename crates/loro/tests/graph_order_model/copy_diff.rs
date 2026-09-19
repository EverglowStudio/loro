use super::support::*;
use loro::{ContainerTrait, ExportMode, LoroValue};

#[test]
fn partial_copy_diff_preserves_equal_key_order_across_reallocated_edge_identities() {
    // The first case is P1-01. The mirror moves the fresh identity across B in
    // the opposite direction, without changing the ordinary copy-diff contract.
    for (source_peer, target_peer, expected_names) in [(2, 20, ["A", "B"]), (19, 2, ["B", "A"])] {
        let seed = doc(1);
        let parent = seed.get_graph("g").create_node().unwrap();
        seed.get_graph("g")
            .node_meta(parent)
            .unwrap()
            .insert("name", "P")
            .unwrap();
        seed.commit();
        let source = copy(&seed, source_peer);
        let remote = copy(&seed, 9);
        let g = source.get_graph("g");
        let h = remote.get_graph("g");
        g.configure_order_jitter(0);
        h.configure_order_jitter(0);
        let a = g.create_edge(parent, parent).unwrap();
        g.edge_meta(a).unwrap().insert("name", "A").unwrap();
        let b = h.create_edge(parent, parent).unwrap();
        h.edge_meta(b).unwrap().insert("name", "B").unwrap();
        h.edge_meta(b).unwrap().insert("keep", 42).unwrap();
        source.commit();
        remote.commit();
        assert_eq!(g.edge_record(a).unwrap().position.as_bytes(), &[0x80]);
        assert_eq!(h.edge_record(b).unwrap().position.as_bytes(), &[0x80]);

        let v0 = remote.state_frontiers();
        let target = copy(&remote, target_peer);
        let t = target.get_graph("g");
        let b_meta_id = t.edge_meta(b).unwrap().id();
        let b_meta = t.edge_meta(b).unwrap().get_deep_value();
        let parent_meta_id = t.node_meta(parent).unwrap().id();
        let parent_meta = t.node_meta(parent).unwrap().get_deep_value();
        assert_eq!(order(&t, parent), vec![b]);
        source
            .import(&remote.export(ExportMode::all_updates()).unwrap())
            .unwrap();
        let v1 = source.state_frontiers();
        assert_eq!(
            order(&g, parent),
            if source_peer < 9 {
                vec![a, b]
            } else {
                vec![b, a]
            }
        );

        target.apply_diff(source.diff(&v0, &v1).unwrap()).unwrap();
        target.commit();
        let rows = t.ordered_out_edges(parent).unwrap();
        let names: Vec<_> = rows
            .iter()
            .map(|row| {
                t.edge_meta(row.edge_id)
                    .unwrap()
                    .get("name")
                    .unwrap()
                    .get_deep_value()
            })
            .collect();
        assert_eq!(
            names,
            expected_names.map(LoroValue::from).to_vec(),
            "copying position alone must not let the new identity reverse the source order"
        );
        let a_index = expected_names.iter().position(|name| *name == "A").unwrap();
        let copied_a = rows[a_index].edge_id;
        assert_ne!(copied_a, a);
        assert_ne!(copied_a, b);
        assert_eq!(copied_a.peer, target_peer);
        assert_eq!(rows[1 - a_index].edge_id, b);
        assert_eq!(t.node_count(), 1);
        assert_eq!(t.edge_count(), 2);
        for row in &rows {
            let record = t.get_edge(row.edge_id).unwrap();
            assert_eq!((record.source, record.target), (parent, parent));
        }
        assert_eq!(t.edge_meta(b).unwrap().id(), b_meta_id);
        assert_eq!(t.edge_meta(b).unwrap().get_deep_value(), b_meta);
        assert_eq!(t.node_meta(parent).unwrap().id(), parent_meta_id);
        assert_eq!(t.node_meta(parent).unwrap().get_deep_value(), parent_meta);
        check(&target, "g", &history(&target));
    }
}
