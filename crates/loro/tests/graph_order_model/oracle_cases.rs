//! These executable contracts pin the model before comparing any native result.
use super::oracle::{Fact, History, Id, Kind};

const GRAPH: &str = "cid:root-g:Graph";
fn id(peer: u64, counter: i32) -> Id {
    Id { peer, counter }
}
fn put(h: &mut History, peer: u64, counter: i32, lamport: u64, kind: Kind) {
    h.insert(Fact {
        id: id(peer, counter),
        lamport,
        graph: GRAPH.into(),
        kind,
    });
}
fn seed() -> History {
    let mut h = History::default();
    put(&mut h, 1, 0, 0, Kind::Node(id(1, 0)));
    for counter in [1, 2, 3] {
        put(
            &mut h,
            1,
            counter,
            counter as u64,
            Kind::Edge {
                id: id(1, counter),
                source: id(1, 0),
                target: id(1, 0),
                position: vec![0x80],
            },
        );
    }
    h
}

#[test]
fn model_same_peer_ties_use_immutable_counter_not_last_writer() {
    let mut h = seed();
    put(
        &mut h,
        99,
        0,
        10,
        Kind::Order {
            id: id(1, 1),
            position: vec![0x80],
        },
    );
    assert_eq!(
        h.snapshot(GRAPH).ordered(id(1, 0)),
        vec![id(1, 1), id(1, 2), id(1, 3)]
    );
}

#[test]
fn model_writer_clock_is_not_position_or_counter_order() {
    let mut h = seed();
    put(
        &mut h,
        2,
        900,
        20,
        Kind::Order {
            id: id(1, 2),
            position: vec![0xf0, 0x80],
        },
    );
    put(
        &mut h,
        3,
        0,
        20,
        Kind::Order {
            id: id(1, 2),
            position: vec![0x10, 0x80],
        },
    );
    let snapshot = h.snapshot(GRAPH);
    assert_eq!(snapshot.edges[&id(1, 2)].writer, id(3, 0));
    assert_eq!(
        snapshot.ordered(id(1, 0)),
        vec![id(1, 2), id(1, 1), id(1, 3)]
    );
    put(
        &mut h,
        1,
        4,
        21,
        Kind::Order {
            id: id(1, 2),
            position: vec![0x08, 0x80],
        },
    );
    assert_eq!(h.snapshot(GRAPH).edges[&id(1, 2)].writer, id(1, 4));
}

#[test]
fn model_hidden_registers_survive_and_restore_only_named_deletes() {
    let mut h = seed();
    put(&mut h, 2, 0, 10, Kind::DeleteNode(id(1, 0)));
    put(&mut h, 3, 0, 10, Kind::DeleteNode(id(1, 0)));
    put(
        &mut h,
        4,
        0,
        11,
        Kind::Order {
            id: id(1, 2),
            position: vec![0x10, 0x80],
        },
    );
    put(
        &mut h,
        2,
        1,
        12,
        Kind::RestoreNode {
            id: id(1, 0),
            deletes: [id(2, 0)].into(),
        },
    );
    let hidden = h.snapshot(GRAPH);
    assert!(hidden.ordered(id(1, 0)).is_empty());
    assert!(hidden.edges[&id(1, 2)].alive);
    assert_eq!(hidden.edges[&id(1, 2)].position, vec![0x10, 0x80]);
    put(
        &mut h,
        3,
        1,
        13,
        Kind::RestoreNode {
            id: id(1, 0),
            deletes: [id(3, 0)].into(),
        },
    );
    assert_eq!(
        h.snapshot(GRAPH).ordered(id(1, 0)),
        vec![id(1, 2), id(1, 1), id(1, 3)]
    );
}

#[test]
fn model_replay_is_duplicate_and_delivery_order_independent() {
    let h = seed();
    let mut reverse = History::default();
    for fact in h.0.values().rev() {
        reverse.insert(fact.clone());
        reverse.insert(fact.clone());
    }
    assert_eq!(h.snapshot(GRAPH), reverse.snapshot(GRAPH));
    assert!(h.snapshot("another:Graph").edges.is_empty());
}
