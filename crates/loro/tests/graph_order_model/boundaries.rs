use super::support::*;
use loro::{ExportMode, GraphOrderError, GraphOrderTarget as At};

#[test]
fn maximum_length_allocation_errors_leave_target_and_auxiliary_edges_untouched() {
    let smallest = format!("{}80", "00".repeat(4095));
    let prefix = "00".repeat(4094);
    let left = format!("{prefix}8080");
    let right = format!("{prefix}8180");
    for (keys, collision) in [
        (vec![smallest.as_str(), "c080"], false),
        (vec![left.as_str(), left.as_str(), right.as_str()], true),
    ] {
        let (d, n, e) = fixture(&keys);
        let g = d.get_graph("g");
        let before = json(&d);
        let state = d.get_deep_value();
        let frontiers = d.state_frontiers();
        let at = if collision {
            At::Before(e[1])
        } else {
            At::Start
        };
        assert!(matches!(
            g.create_edge_at(n, n, at),
            Err(GraphOrderError::PositionTooLong)
        ));
        assert_eq!(d.get_pending_txn_len(), 0);
        assert_eq!(d.state_frontiers(), frontiers);
        assert_eq!(json(&d), before);
        assert_eq!(d.get_deep_value(), state);
        assert_eq!(order(&g, n), e);
        let fresh = g.create_edge(n, n).unwrap();
        assert!(
            g.get_edge(fresh).is_some(),
            "failure poisoned subsequent edits"
        );
    }
}

#[test]
fn malformed_position_json_rejects_atomically_without_panicking() {
    let (source, _, _) = fixture(&["80"]);
    for key in [
        String::new(),
        "8".into(),
        "zz".into(),
        "8081".into(),
        "80".repeat(4097),
    ] {
        let mut schema = json(&source);
        for change in schema["changes"].as_array_mut().unwrap() {
            for op in change["ops"].as_array_mut().unwrap() {
                if let Some(body) = op["content"].get_mut("create_edge") {
                    body["position"] = key.clone().into();
                }
            }
        }
        let receiver = doc(99);
        receiver.get_map("existing").insert("keep", true).unwrap();
        receiver.commit();
        let version = receiver.oplog_vv();
        let state = receiver.get_deep_value();
        assert!(receiver
            .import_json_updates(serde_json::to_string(&schema).unwrap())
            .is_err());
        assert_eq!(receiver.oplog_vv(), version);
        assert_eq!(receiver.get_deep_value(), state);
        receiver
            .get_map("existing")
            .insert("still-usable", true)
            .unwrap();
        receiver.commit();
    }
}

#[test]
fn binary_order_payloads_reject_truncation_trailing_bytes_and_invalid_terminators() {
    // This is a codec-boundary check only; the oracle never imports GraphOp.
    use loro::{JsonOpContent, ID};
    use loro_internal::container::graph::GraphOp;
    let (d, _, e) = fixture(&["4080", "c080"]);
    d.get_graph("g").reorder_edge(e[0], At::End).unwrap();
    d.commit();
    let schema = d.export_json_updates_without_peer_compression(&Default::default(), &d.oplog_vv());
    let mut checked = 0;
    for change in schema.changes {
        for op in change.ops {
            let JsonOpContent::Graph(graph) = op.content else {
                continue;
            };
            if !matches!(
                graph,
                GraphOp::CreateEdge { .. } | GraphOp::SetEdgeOrder { .. }
            ) {
                continue;
            }
            let id = ID::new(change.id.peer, op.counter);
            let encoded = graph.encoded();
            assert_eq!(GraphOp::decode(&encoded, id).unwrap(), graph);
            for end in 0..encoded.len() {
                assert!(
                    GraphOp::decode(&encoded[..end], id).is_err(),
                    "accepted truncated order payload {end}"
                );
            }
            let mut trailing = encoded.clone();
            trailing.push(0);
            assert!(GraphOp::decode(&trailing, id).is_err());
            let mut invalid = encoded;
            *invalid.last_mut().unwrap() = 0;
            assert!(GraphOp::decode(&invalid, id).is_err());
            checked += 1;
        }
    }
    assert_eq!(checked, 3);
}

#[test]
fn equal_keys_use_numeric_counter_order_past_a_decimal_boundary() {
    let (d, n, e) = fixture(&["80"; 13]);
    assert_eq!(order(&d.get_graph("g"), n), e);
    assert_eq!(e[0].peer, e[12].peer);
    assert!(e[0].counter < 10 && e[12].counter > 10);
    let receiver = doc(u64::MAX - 1);
    receiver
        .import(&d.export(ExportMode::all_updates()).unwrap())
        .unwrap();
    assert_eq!(order(&receiver.get_graph("g"), n), e);
    check(&receiver, "g", &history(&d));
}

#[test]
fn generated_positions_survive_later_anchor_moves_and_concurrent_deletes() {
    let (a, n, e) = fixture(&["4080", "80", "c080"]);
    let b = copy(&a, 23);
    let g = a.get_graph("g");
    let x = g.create_edge_at(n, n, At::After(e[0])).unwrap();
    let position = g.edge_record(x).unwrap().position;
    g.reorder_edge(e[0], At::End).unwrap();
    a.commit();
    assert_eq!(order(&g, n), vec![x, e[1], e[2], e[0]]);
    assert_eq!(g.edge_record(x).unwrap().position, position);
    b.get_graph("g").delete_edge(e[0]).unwrap();
    b.commit();
    sync(&a, &b);
    assert_eq!(order(&g, n), vec![x, e[1], e[2]]);
    assert_eq!(g.edge_record(x).unwrap().position, position);
    g.restore_edge(e[0]).unwrap();
    assert_eq!(order(&g, n), vec![x, e[1], e[2], e[0]]);
}

#[test]
fn three_replicas_each_make_multiple_offline_moves_before_merging() {
    let (seed, n, e) = fixture(&["2080", "4080", "8080", "c080"]);
    let replicas = [copy(&seed, 2), copy(&seed, 3), copy(&seed, (1 << 54) + 4)];
    let mut expected = history(&seed);
    for (index, d) in replicas.iter().enumerate() {
        let g = d.get_graph("g");
        g.reorder_edge(e[index], At::End).unwrap();
        d.commit();
        let mut intent = e.clone();
        intent.remove(index);
        intent.push(e[index]);
        assert_eq!(order(&g, n), intent);
        g.reorder_edge(e[3], At::Start).unwrap();
        d.commit();
        intent.retain(|id| *id != e[3]);
        intent.insert(0, e[3]);
        assert_eq!(order(&g, n), intent);
        expected.extend(&history(d));
    }
    let packets: Vec<_> = replicas
        .iter()
        .map(|d| d.export(ExportMode::all_updates()).unwrap())
        .collect();
    for (r, schedule) in [[1, 2, 0], [2, 0, 1], [0, 1, 2]].iter().enumerate() {
        for i in schedule {
            replicas[r].import(&packets[*i]).unwrap();
        }
        check(&replicas[r], "g", &expected);
    }
    let merged = order(&replicas[0].get_graph("g"), n);
    for d in &replicas {
        assert_eq!(order(&d.get_graph("g"), n), merged);
    }
}
