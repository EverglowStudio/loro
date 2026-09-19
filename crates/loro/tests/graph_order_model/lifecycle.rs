use super::support::*;
use loro::{ExportMode, GraphOrderTarget as At};

#[test]
fn hidden_order_facts_survive_both_delivery_orders_and_endpoint_restore() {
    for delete_endpoint in [false, true] {
        let (seed, n, e) = fixture(&["4080", "80", "c080"]);
        let deletion = copy(&seed, 2);
        let movement = copy(&seed, 3);
        if delete_endpoint {
            deletion.get_graph("g").delete_node(n).unwrap();
        } else {
            deletion.get_graph("g").delete_edge(e[1]).unwrap();
        }
        movement
            .get_graph("g")
            .reorder_edge(e[1], At::Start)
            .unwrap();
        deletion.commit();
        movement.commit();
        let moved = movement.get_graph("g").edge_record(e[1]).unwrap();
        let mut expected = history(&deletion);
        expected.extend(&history(&movement));
        let packets = [
            deletion.export(ExportMode::all_updates()).unwrap(),
            movement.export(ExportMode::all_updates()).unwrap(),
        ];
        for order in [[0, 1], [1, 0]] {
            let receiver = copy(&seed, 4);
            for index in order {
                receiver.import(&packets[index]).unwrap();
            }
            let g = receiver.get_graph("g");
            check(&receiver, "g", &expected);
            let hidden = g.edge_record(e[1]).unwrap();
            assert!(!hidden.visible);
            assert_eq!(hidden.alive, delete_endpoint);
            assert_eq!(hidden.position, moved.position);
            assert_eq!(hidden.last_order, moved.last_order);
            assert_eq!(g.index_of_out_edge(e[1]), None);
            if delete_endpoint {
                g.restore_node(n).unwrap();
            } else {
                g.restore_edge(e[1]).unwrap();
            }
            assert_eq!(super::support::order(&g, n), vec![e[1], e[0], e[2]]);
            let recreated = g.create_edge(n, n).unwrap();
            assert_ne!(recreated, e[1]);
            assert_eq!(
                super::support::order(&g, n),
                vec![e[1], e[0], e[2], recreated]
            );
            receiver.commit();
            check(&receiver, "g", &history(&receiver));
        }
    }
}

#[test]
fn order_does_not_restore_an_independent_edge_delete_when_an_endpoint_returns() {
    let (seed, n, e) = fixture(&["4080", "80", "c080"]);
    let a = copy(&seed, 2);
    let b = copy(&seed, 3);
    a.get_graph("g").delete_node(n).unwrap();
    a.get_graph("g").delete_edge(e[1]).unwrap();
    b.get_graph("g").reorder_edge(e[1], At::Start).unwrap();
    a.commit();
    b.commit();
    sync(&a, &b);
    let g = a.get_graph("g");
    let saved = g.edge_record(e[1]).unwrap().position;
    g.restore_node(n).unwrap();
    assert_eq!(order(&g, n), vec![e[0], e[2]]);
    assert_eq!(g.edge_record(e[1]).unwrap().position, saved);
    g.restore_edge(e[1]).unwrap();
    assert_eq!(order(&g, n), vec![e[1], e[0], e[2]]);
}

#[test]
fn ordering_and_node_edge_metadata_are_independent_fields() {
    let (seed, n, e) = fixture(&["4080", "80", "c080"]);
    let a = copy(&seed, 2);
    let b = copy(&seed, 3);
    a.get_graph("g").reorder_edge(e[1], At::Start).unwrap();
    a.commit();
    let moved = a.get_graph("g").edge_record(e[1]).unwrap();
    b.get_graph("g")
        .edge_meta(e[1])
        .unwrap()
        .insert("title", "edge")
        .unwrap();
    b.get_graph("g")
        .node_meta(n)
        .unwrap()
        .insert("title", "node")
        .unwrap();
    b.commit();
    sync(&a, &b);
    for d in [&a, &b] {
        let g = d.get_graph("g");
        assert_eq!(order(&g, n), vec![e[1], e[0], e[2]]);
        assert_eq!(g.edge_record(e[1]).unwrap().last_order, moved.last_order);
        assert_eq!(
            g.edge_meta(e[1])
                .unwrap()
                .get("title")
                .unwrap()
                .get_deep_value(),
            "edge".into()
        );
        assert_eq!(
            g.node_meta(n)
                .unwrap()
                .get("title")
                .unwrap()
                .get_deep_value(),
            "node".into()
        );
    }
}
