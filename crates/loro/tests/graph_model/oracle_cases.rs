use super::oracle::*;

const G: Graph = Graph(0);

fn node(oracle: &mut Oracle, at: &mut Version, graph: Graph) -> NodeId {
    NodeId {
        graph,
        birth: oracle.append_to(at, Action::CreateNode(graph)),
    }
}

fn edge(oracle: &mut Oracle, at: &mut Version, source: NodeId, target: NodeId) -> EdgeId {
    EdgeId {
        graph: source.graph,
        birth: oracle.append_to(
            at,
            Action::CreateEdge {
                graph: source.graph,
                source,
                target,
            },
        ),
    }
}

#[test]
fn oracle_partial_restore_does_not_cover_an_unobserved_delete() {
    let mut oracle = Oracle::default();
    let mut a = Version::new();
    let n = node(&mut oracle, &mut a, G);
    let created = a.clone();
    let mut b = a.clone();
    oracle.append_to(&mut a, Action::Delete(Object::Node(n)));
    oracle.append_to(&mut b, Action::Delete(Object::Node(n)));
    let deleted = a.clone();
    oracle.append_to(&mut a, Action::Restore(Object::Node(n)));
    let partial_restore = a.clone();
    assert!(oracle.snapshot(G, &a).nodes[&n].alive);

    a.extend(b);
    let both = a.clone();
    assert!(!oracle.snapshot(G, &both).nodes[&n].alive);
    // Extra property writes cannot make the lower-numbered concurrent Delete
    // disappear from the set of maximal LIFECYCLE events.
    for index in 0..5 {
        oracle.append_to(
            &mut a,
            Action::Set {
                object: Object::Node(n),
                key: "padding".into(),
                value: index.to_string(),
            },
        );
    }
    assert!(!oracle.snapshot(G, &a).nodes[&n].alive);
    oracle.append_to(&mut a, Action::Restore(Object::Node(n)));
    assert!(oracle.snapshot(G, &a).nodes[&n].alive);

    // An oracle must reevaluate the requested boundary, not its latest state.
    assert!(oracle.snapshot(G, &created).nodes[&n].alive);
    assert!(!oracle.snapshot(G, &deleted).nodes[&n].alive);
    assert!(oracle.snapshot(G, &partial_restore).nodes[&n].alive);
    assert!(!oracle.snapshot(G, &both).nodes[&n].alive);
}

#[test]
fn oracle_later_event_number_is_not_happens_before() {
    let mut oracle = Oracle::default();
    let mut a = Version::new();
    let n = node(&mut oracle, &mut a, G);
    oracle.append_to(&mut a, Action::Delete(Object::Node(n)));
    let mut b = a.clone();
    let mut c = a.clone();
    oracle.append_to(&mut b, Action::Delete(Object::Node(n)));
    oracle.append_to(&mut c, Action::Restore(Object::Node(n)));
    let restored = c.clone();
    c.extend(b);
    assert!(!oracle.snapshot(G, &c).nodes[&n].alive);
    assert!(oracle.snapshot(G, &restored).nodes[&n].alive);
}

#[test]
fn oracle_endpoint_restore_reveals_only_edges_with_live_lifecycles() {
    let mut oracle = Oracle::default();
    let mut a = Version::new();
    let x = node(&mut oracle, &mut a, G);
    let y = node(&mut oracle, &mut a, G);
    let isolated = node(&mut oracle, &mut a, G);
    let e1 = edge(&mut oracle, &mut a, x, y);
    let e2 = edge(&mut oracle, &mut a, x, y);
    let reverse = edge(&mut oracle, &mut a, y, x);
    let self_loop = edge(&mut oracle, &mut a, x, x);
    let original = a.clone();
    let mut offline = a.clone();
    oracle.append_to(&mut a, Action::Delete(Object::Edge(e2)));
    oracle.append_to(&mut a, Action::Delete(Object::Node(x)));
    let unseen_edge = edge(&mut oracle, &mut offline, y, x);
    a.extend(offline);
    let hidden = oracle.snapshot(G, &a);
    assert!(hidden.nodes[&y].alive && hidden.nodes[&isolated].alive);
    assert!(hidden.visible_edges().is_empty());
    assert!(hidden.edges[&e1].alive);
    assert!(hidden.edges[&unseen_edge].alive);
    assert!(!hidden.edges[&e2].alive);
    assert!(hidden.incoming(x).is_empty() && hidden.outgoing(x).is_empty());

    oracle.append_to(&mut a, Action::Restore(Object::Node(x)));
    let live = oracle.snapshot(G, &a);
    assert_eq!(
        live.visible_edges(),
        [e1, reverse, self_loop, unseen_edge].into()
    );
    assert_eq!(live.outgoing(x), [e1, self_loop].into());
    assert_eq!(live.incoming(x), [reverse, self_loop, unseen_edge].into());
    assert_eq!(live.successors(x), [x, y].into());
    assert_eq!(live.predecessors(x), [x, y].into());
    assert_eq!(live.visible_nodes(), [x, y, isolated].into());
    assert_eq!(
        oracle.snapshot(G, &original).outgoing(x),
        [e1, e2, self_loop].into()
    );
}

#[test]
fn oracle_nested_text_and_scalar_writes_do_not_restore_objects() {
    let mut oracle = Oracle::default();
    let mut a = Version::new();
    let n = node(&mut oracle, &mut a, G);
    let e = edge(&mut oracle, &mut a, n, n);
    for object in [Object::Node(n), Object::Edge(e)] {
        oracle.append_to(
            &mut a,
            Action::SeedText {
                object,
                key: "body".into(),
                value: "中心".into(),
            },
        );
    }
    let baseline = a.clone();
    let mut b = a.clone();
    let mut c = a.clone();
    for object in [Object::Node(n), Object::Edge(e)] {
        oracle.append_to(&mut a, Action::Delete(object));
        oracle.append_to(
            &mut b,
            Action::PrefixText {
                object,
                key: "body".into(),
                value: "左/".into(),
            },
        );
        oracle.append_to(
            &mut c,
            Action::SuffixText {
                object,
                key: "body".into(),
                value: "/右".into(),
            },
        );
    }
    a.extend(b);
    a.extend(c);
    oracle.append_to(
        &mut a,
        Action::Set {
            object: Object::Node(n),
            key: "hidden".into(),
            value: "retained".into(),
        },
    );
    let hidden = oracle.snapshot(G, &a);
    assert!(!hidden.nodes[&n].alive && !hidden.edges[&e].alive);
    for object in [Object::Node(n), Object::Edge(e)] {
        assert_eq!(hidden.properties(object).texts["body"], "左/中心/右");
        assert_eq!(
            oracle.snapshot(G, &baseline).properties(object).texts["body"],
            "中心"
        );
    }
    oracle.append_to(&mut a, Action::Restore(Object::Node(n)));
    assert!(!oracle.snapshot(G, &a).edges[&e].visible);
    oracle.append_to(&mut a, Action::Restore(Object::Edge(e)));
    assert!(oracle.snapshot(G, &a).edges[&e].visible);
    assert_eq!(
        oracle.snapshot(G, &a).nodes[&n].properties.scalars["hidden"],
        "retained"
    );
}

#[test]
fn oracle_pending_packets_are_not_a_historical_version() {
    let mut oracle = Oracle::default();
    let mut a = Version::new();
    let n = node(&mut oracle, &mut a, G);
    let create = n.birth;
    let delete = oracle.append_to(&mut a, Action::Delete(Object::Node(n)));
    let restore = oracle.append_to(&mut a, Action::Restore(Object::Node(n)));
    for order in [
        [restore, delete, create],
        [delete, create, restore],
        [create, restore, delete],
    ] {
        let mut received = Version::new();
        for event in order {
            received.insert(event);
            received.insert(event); // duplicate delivery
            let closed = oracle.closed_subset(&received);
            assert!(oracle.is_closed(&closed));
            let state = oracle.snapshot(G, &closed);
            if closed.contains(&create) {
                assert_eq!(
                    state.nodes[&n].alive,
                    !closed.contains(&delete) || closed.contains(&restore)
                );
            } else {
                assert!(state.nodes.is_empty());
            }
        }
        assert_eq!(received, a);
    }
    assert!(!oracle.is_closed(&[restore].into()));
}

#[test]
fn oracle_identity_is_typed_scoped_and_not_a_property_value() {
    let mut oracle = Oracle::default();
    let mut at = Version::new();
    let old = node(&mut oracle, &mut at, G);
    let other = node(&mut oracle, &mut at, Graph(1));
    let old_edge = edge(&mut oracle, &mut at, old, old);
    oracle.append_to(&mut at, Action::Delete(Object::Node(old)));
    let new = node(&mut oracle, &mut at, G);
    for id in [old, new] {
        oracle.append_to(
            &mut at,
            Action::Set {
                object: Object::Node(id),
                key: "name".into(),
                value: "same business key".into(),
            },
        );
    }
    let state = oracle.snapshot(G, &at);
    assert_eq!(state.edges[&old_edge].source, old);
    assert!(!state.edges[&old_edge].visible);
    assert_eq!(state.visible_nodes(), [new].into());
    assert_eq!(
        oracle.snapshot(Graph(1), &at).visible_nodes(),
        [other].into()
    );
    assert_eq!(
        oracle.validate(
            &at,
            &Action::CreateEdge {
                graph: G,
                source: new,
                target: other
            }
        ),
        Err("cross-graph endpoint")
    );
    let forged = NodeId {
        graph: G,
        birth: other.birth,
    };
    assert_eq!(
        oracle.validate(
            &at,
            &Action::CreateEdge {
                graph: G,
                source: new,
                target: forged
            }
        ),
        Err("missing endpoint")
    );
    assert_eq!(
        oracle.validate(
            &at,
            &Action::Delete(Object::Node(NodeId {
                graph: G,
                birth: old_edge.birth
            }))
        ),
        Err("missing object")
    );
}

#[test]
fn oracle_property_overwrite_and_remove_respect_history() {
    let mut oracle = Oracle::default();
    let mut at = Version::new();
    let n = node(&mut oracle, &mut at, G);
    for value in ["first", "second"] {
        oracle.append_to(
            &mut at,
            Action::Set {
                object: Object::Node(n),
                key: "writer/1".into(),
                value: value.into(),
            },
        );
    }
    let before_remove = at.clone();
    oracle.append_to(
        &mut at,
        Action::Remove {
            object: Object::Node(n),
            key: "writer/1".into(),
        },
    );
    assert!(oracle.snapshot(G, &at).nodes[&n]
        .properties
        .scalars
        .is_empty());
    assert_eq!(
        oracle.snapshot(G, &before_remove).nodes[&n]
            .properties
            .scalars["writer/1"],
        "second"
    );
}
