use super::{oracle::History, support::*};
use loro::{GraphOrderTarget as At, LoroDoc};

fn merged_history(a: &LoroDoc, b: &LoroDoc) -> History {
    let mut expected = history(a);
    expected.extend(&history(b));
    expected
}

#[test]
fn concurrent_same_edge_uses_lamport_peer_not_largest_position() {
    let (seed, n, e) = fixture(&["4080", "80", "c080"]);
    let a = copy(&seed, 2);
    let b = copy(&seed, 3);
    a.get_graph("g").reorder_edge(e[1], At::End).unwrap();
    b.get_graph("g").reorder_edge(e[1], At::Start).unwrap();
    a.commit();
    b.commit();
    let left = a.get_graph("g").edge_record(e[1]).unwrap();
    let right = b.get_graph("g").edge_record(e[1]).unwrap();
    assert_eq!(left.last_order.lamport, right.last_order.lamport);
    assert!(right.position < left.position);
    let expected = merged_history(&a, &b);
    let c = copy(&seed, 4);
    sync(&a, &b);
    sync(&c, &b);
    for d in [&a, &b, &c] {
        assert_eq!(order(&d.get_graph("g"), n), vec![e[1], e[0], e[2]]);
        assert_eq!(
            d.get_graph("g").edge_record(e[1]).unwrap().last_order,
            right.last_order
        );
        check(d, "g", &expected);
    }
    // A causally newer move wins even from the lower peer and with a smaller key.
    a.get_graph("g").reorder_edge(e[2], At::Start).unwrap();
    a.get_graph("g").reorder_edge(e[1], At::Start).unwrap();
    a.commit();
    let newer = a.get_graph("g").edge_record(e[1]).unwrap();
    assert!(newer.position < right.position);
    sync(&a, &b);
    assert_eq!(
        b.get_graph("g").edge_record(e[1]).unwrap().last_order,
        newer.last_order
    );
}

#[test]
fn independent_edges_and_multi_parent_associations_merge_separately() {
    let (seed, n, e) = fixture(&["2080", "4080", "8080", "c080"]);
    let g = seed.get_graph("g");
    let q = g.create_node().unwrap();
    let qn = g.create_edge(q, n).unwrap();
    let qq = g.create_edge(q, q).unwrap();
    seed.commit();
    let a = copy(&seed, 2);
    let b = copy(&seed, 3);
    a.get_graph("g").reorder_edge(e[0], At::End).unwrap();
    b.get_graph("g").reorder_edge(e[3], At::Start).unwrap();
    b.get_graph("g").reorder_edge(qn, At::End).unwrap();
    a.commit();
    b.commit();
    let expected = merged_history(&a, &b);
    sync(&a, &b);
    for d in [&a, &b] {
        assert_eq!(order(&d.get_graph("g"), n), vec![e[3], e[1], e[2], e[0]]);
        assert_eq!(order(&d.get_graph("g"), q), vec![qq, qn]);
        check(d, "g", &expected);
    }
}

#[test]
fn concurrent_same_gap_insertions_use_full_numeric_edge_identity() {
    let (seed, n, e) = fixture(&["4080", "c080"]);
    let a = copy(&seed, 2);
    let b = copy(&seed, (1 << 54) + 3);
    let x = a
        .get_graph("g")
        .create_edge_at(n, n, At::Before(e[1]))
        .unwrap();
    let y = b
        .get_graph("g")
        .create_edge_at(n, n, At::Before(e[1]))
        .unwrap();
    a.commit();
    b.commit();
    assert_eq!(
        a.get_graph("g").edge_record(x).unwrap().position,
        b.get_graph("g").edge_record(y).unwrap().position
    );
    let expected = merged_history(&a, &b);
    sync(&a, &b);
    assert_eq!(order(&a.get_graph("g"), n), vec![e[0], x, y, e[1]]);
    check(&a, "g", &expected);
    check(&b, "g", &expected);
    let z = a
        .get_graph("g")
        .create_edge_at(n, n, At::Before(y))
        .unwrap();
    assert_eq!(order(&a.get_graph("g"), n), vec![e[0], x, z, y, e[1]]);
}

#[test]
fn auxiliary_writes_compete_normally_and_can_lose_individually() {
    let (seed, n, e) = fixture(&["80", "80", "80", "c080"]);
    let a = copy(&seed, 2);
    let b = copy(&seed, 3);
    let inserted = a
        .get_graph("g")
        .create_edge_at(n, n, At::Before(e[1]))
        .unwrap();
    a.commit();
    // Advance only b's local clock, without observing a's collision batch.
    for i in 0..12 {
        b.get_graph("g")
            .edge_meta(e[1])
            .unwrap()
            .insert("tick", i)
            .unwrap();
    }
    b.get_graph("g").reorder_edge(e[1], At::Start).unwrap();
    b.commit();
    let remote = b.get_graph("g").edge_record(e[1]).unwrap();
    let auxiliary = a.get_graph("g").edge_record(e[2]).unwrap();
    let expected = merged_history(&a, &b);
    sync(&a, &b);
    for d in [&a, &b] {
        let g = d.get_graph("g");
        assert_eq!(g.edge_record(e[1]).unwrap().last_order, remote.last_order);
        assert_eq!(
            g.edge_record(e[2]).unwrap().last_order,
            auxiliary.last_order
        );
        assert_eq!(order(&g, n), vec![e[1], e[0], inserted, e[2], e[3]]);
        check(d, "g", &expected);
    }
    let fresh = a
        .get_graph("g")
        .create_edge_at(n, n, At::After(inserted))
        .unwrap();
    assert_eq!(
        order(&a.get_graph("g"), n),
        vec![e[1], e[0], inserted, fresh, e[2], e[3]]
    );
    sync(&a, &b);
    check(&b, "g", &history(&a));
}

#[test]
fn two_concurrent_collision_batches_remain_editable_without_import_repair() {
    let (seed, n, e) = fixture(&["80", "80", "80", "c080"]);
    let a = copy(&seed, 2);
    let b = copy(&seed, 3);
    let x = a
        .get_graph("g")
        .create_edge_at(n, n, At::Before(e[1]))
        .unwrap();
    let y = b
        .get_graph("g")
        .create_edge_at(n, n, At::Before(e[1]))
        .unwrap();
    a.commit();
    b.commit();
    let expected = merged_history(&a, &b);
    sync(&a, &b);
    assert_eq!(
        order(&a.get_graph("g"), n),
        vec![e[0], x, y, e[1], e[2], e[3]]
    );
    for d in [&a, &b] {
        assert_eq!(d.get_pending_txn_len(), 0);
        check(d, "g", &expected);
    }
    let at_merge = a.oplog_vv();
    sync(&a, &b);
    assert_eq!(a.oplog_vv(), at_merge);
    assert_eq!(b.oplog_vv(), at_merge);
    let z = a
        .get_graph("g")
        .create_edge_at(n, n, At::Before(y))
        .unwrap();
    assert_eq!(
        order(&a.get_graph("g"), n),
        vec![e[0], x, z, y, e[1], e[2], e[3]]
    );
}
