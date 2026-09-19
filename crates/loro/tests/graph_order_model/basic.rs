use super::support::*;
use loro::{
    ContainerTrait, GraphEdgeId, GraphNodeId, GraphOrderError, GraphOrderTarget as At,
    GraphPosition,
};

#[test]
fn empty_single_and_noop_operations_do_not_allocate_or_commit() {
    let d = doc(1);
    let g = d.get_graph("g");
    let n = g.create_node().unwrap();
    d.commit();
    assert_eq!(order(&g, n), vec![]);
    assert!(g.out_edge_at(n, 0).is_none());
    let e = g.create_edge(n, n).unwrap();
    d.commit();
    let vv = d.oplog_vv();
    let frontiers = d.state_frontiers();
    for target in [At::Start, At::End, At::Before(e), At::After(e)] {
        let outcome = g.reorder_edge(e, target).unwrap();
        assert!(!outcome.changed);
        assert_eq!(outcome.auxiliary_updates, 0);
        assert_eq!(order(&g, n), vec![e]);
        assert_eq!(d.get_pending_txn_len(), 0);
        assert_eq!(d.oplog_vv(), vv);
        assert_eq!(d.state_frontiers(), frontiers);
    }
    let second = g.create_edge(n, n).unwrap();
    assert_eq!(second.counter, e.counter + 1, "no-op consumed an identity");
    let pending = d.get_pending_txn_len();
    assert!(pending > 0);
    let pending_frontiers = d.state_frontiers();
    let pending_vv = d.oplog_vv();
    assert!(!g.reorder_edge(second, At::End).unwrap().changed);
    assert_eq!(order(&g, n), vec![e, second]);
    assert_eq!(g.out_edge_at(n, 1).unwrap().edge_id, second);
    assert_eq!(g.index_of_out_edge(second), Some(1));
    g.edge_record(second).unwrap();
    g.get_value();
    g.get_deep_value();
    assert_eq!(
        d.get_pending_txn_len(),
        pending,
        "query/no-op committed the transaction"
    );
    assert_eq!(d.oplog_vv(), pending_vv);
    assert_eq!(d.state_frontiers(), pending_frontiers);
}

#[test]
fn create_and_reorder_intent_removes_the_moving_edge_before_finding_gap() {
    let d = doc(1);
    let g = d.get_graph("g");
    let n = g.create_node().unwrap();
    let a = g.create_edge(n, n).unwrap();
    let c = g.create_edge(n, n).unwrap();
    let b = g.create_edge_at(n, n, At::Before(c)).unwrap();
    let first = g.create_edge_at(n, n, At::Start).unwrap();
    let last = g.create_edge_at(n, n, At::End).unwrap();
    assert_eq!(order(&g, n), vec![first, a, b, c, last]);
    assert!(g.reorder_edge(a, At::After(c)).unwrap().changed);
    assert_eq!(order(&g, n), vec![first, b, c, a, last]);
    assert!(g.reorder_edge(last, At::Before(b)).unwrap().changed);
    assert_eq!(order(&g, n), vec![first, last, b, c, a]);
    assert!(g.reorder_edge(c, At::Before(b)).unwrap().changed);
    assert_eq!(order(&g, n), vec![first, last, c, b, a]);
    d.commit();
    let vv = d.oplog_vv();
    for (e, at) in [
        (first, At::Start),
        (a, At::End),
        (c, At::After(last)),
        (c, At::Before(b)),
        (c, At::Before(c)),
        (c, At::After(c)),
    ] {
        assert!(!g.reorder_edge(e, at).unwrap().changed);
    }
    assert_eq!(d.get_pending_txn_len(), 0);
    assert_eq!(d.oplog_vv(), vv);
    check(&d, "g", &history(&d));
}

#[test]
fn source_domains_parallel_edges_cycles_and_metadata_keep_identity() {
    let d = doc(1);
    let g = d.get_graph("g");
    let p = g.create_node().unwrap();
    let q = g.create_node().unwrap();
    let x = g.create_node().unwrap();
    let px = g.create_edge(p, x).unwrap();
    let parallel = g.create_edge(p, x).unwrap();
    let pq = g.create_edge(p, q).unwrap();
    let qx = g.create_edge(q, x).unwrap();
    let qp = g.create_edge(q, p).unwrap();
    let cycle = g.create_edge(x, p).unwrap();
    let self_loop = g.create_edge(p, p).unwrap();
    let meta = g.edge_meta(px).unwrap();
    let node_meta = g.node_meta(x).unwrap();
    meta.insert("weight", 9).unwrap();
    node_meta.insert("title", "shared").unwrap();
    let identities = (meta.id(), node_meta.id());
    g.reorder_edge(px, At::End).unwrap();
    assert_eq!(order(&g, p), vec![parallel, pq, self_loop, px]);
    assert_eq!(order(&g, q), vec![qx, qp]);
    assert_eq!(order(&g, x), vec![cycle]);
    assert_eq!(g.edge_count(), 7);
    assert_eq!(g.node_count(), 3);
    assert_eq!(
        (g.edge_meta(px).unwrap().id(), g.node_meta(x).unwrap().id()),
        identities
    );
    assert_eq!(meta.get("weight").unwrap().get_deep_value(), 9.into());
    assert_eq!(
        node_meta.get("title").unwrap().get_deep_value(),
        "shared".into()
    );
    assert_eq!(
        g.outgoing_edges(p).unwrap(),
        vec![px, parallel, pq, self_loop]
    );
    d.commit();
    check(&d, "g", &history(&d));
}

#[test]
fn invalid_scope_anchor_and_hidden_edge_errors_are_atomic() {
    let d = doc(1);
    let g = d.get_graph("g");
    let n = g.create_node().unwrap();
    let m = g.create_node().unwrap();
    let e = g.create_edge(n, m).unwrap();
    let other_source = g.create_edge(m, n).unwrap();
    let hidden = g.create_edge(n, n).unwrap();
    g.delete_edge(hidden).unwrap();
    let other = d.get_graph("other");
    let foreign = other.create_node().unwrap();
    let foreign_edge = other.create_edge(foreign, foreign).unwrap();
    d.commit();
    let vv = d.oplog_vv();
    let frontiers = d.state_frontiers();
    let before = d.get_deep_value();
    for at in [At::Before(other_source), At::After(other_source)] {
        assert!(matches!(
            g.reorder_edge(e, at),
            Err(GraphOrderError::CrossSource)
        ));
        assert!(matches!(
            g.create_edge_at(n, n, at),
            Err(GraphOrderError::CrossSource)
        ));
    }
    for anchor in [hidden, foreign_edge, GraphEdgeId::new(99, 99)] {
        assert!(matches!(
            g.reorder_edge(e, At::Before(anchor)),
            Err(GraphOrderError::AnchorNotVisible)
        ));
        assert!(matches!(
            g.create_edge_at(n, n, At::After(anchor)),
            Err(GraphOrderError::AnchorNotVisible)
        ));
    }
    for edge in [hidden, foreign_edge, GraphEdgeId::new(99, 99)] {
        assert!(matches!(
            g.reorder_edge(edge, At::Start),
            Err(GraphOrderError::EdgeNotVisible)
        ));
        assert_eq!(g.index_of_out_edge(edge), None);
    }
    for missing in [foreign, GraphNodeId::new(99, 99)] {
        assert!(matches!(
            g.create_edge_at(missing, n, At::End),
            Err(GraphOrderError::MissingNode)
        ));
        assert!(matches!(
            g.create_edge_at(n, missing, At::End),
            Err(GraphOrderError::MissingNode)
        ));
    }
    assert_eq!(d.get_pending_txn_len(), 0);
    assert_eq!(d.oplog_vv(), vv);
    assert_eq!(d.state_frontiers(), frontiers);
    assert_eq!(d.get_deep_value(), before);
}

#[test]
fn canonical_position_boundaries_and_variable_length_byte_order() {
    for bytes in [
        vec![0x80],
        vec![0, 0x80],
        vec![0xff, 0x80],
        vec![0x80; 4096],
    ] {
        let parsed = GraphPosition::try_from_bytes(bytes.clone()).unwrap();
        assert_eq!(parsed.as_bytes(), bytes);
        let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(GraphPosition::try_from_hex(&hex).unwrap(), parsed);
    }
    for bytes in [vec![], vec![0], vec![0x80, 0]] {
        assert!(matches!(
            GraphPosition::try_from_bytes(bytes),
            Err(GraphOrderError::InvalidPosition)
        ));
    }
    assert!(matches!(
        GraphPosition::try_from_bytes(vec![0x80; 4097]),
        Err(GraphOrderError::PositionTooLong)
    ));
    for hex in ["", "8", "xx", "00", "8000", "é80"] {
        assert!(matches!(
            GraphPosition::try_from_hex(hex),
            Err(GraphOrderError::InvalidPosition)
        ));
    }
    let (d, n, e) = fixture(&["8180", "80", "800080", "0080"]);
    assert_eq!(order(&d.get_graph("g"), n), vec![e[3], e[1], e[2], e[0]]);
}
