use super::support::*;
use loro::event::{Diff, MapDelta};
use loro::{
    ApplyDiff, ExportMode, GraphDiff, GraphOrderTarget as At, Index, LoroDoc, LoroText, LoroValue,
    Subscription,
};
use std::{
    borrow::Cow,
    sync::{Arc, Mutex},
};

fn owned(diff: Diff<'_>) -> Diff<'static> {
    match diff {
        Diff::Map(map) => Diff::Map(MapDelta {
            updated: map
                .updated
                .into_iter()
                .map(|(key, value)| (Cow::Owned(key.into_owned()), value))
                .collect(),
        }),
        Diff::Graph(graph) => Diff::Graph(Cow::Owned(graph.into_owned())),
        Diff::Tree(tree) => Diff::Tree(Cow::Owned(tree.into_owned())),
        Diff::Text(text) => Diff::Text(text),
        Diff::List(list) => Diff::List(list),
        #[cfg(feature = "counter")]
        Diff::Counter(counter) => Diff::Counter(counter),
        Diff::Unknown => panic!("unexpected unknown event"),
    }
}

struct State {
    value: LoroValue,
    graphs: Vec<GraphDiff>,
    callbacks: usize,
}
struct Mirror {
    state: Arc<Mutex<State>>,
    _subscription: Subscription,
}
impl Mirror {
    fn new(d: &LoroDoc) -> Self {
        let state = Arc::new(Mutex::new(State {
            value: d.get_deep_value(),
            graphs: vec![],
            callbacks: 0,
        }));
        let captured = state.clone();
        // No document or graph handle is captured: only event data rebuilds the mirror.
        let subscription = d.subscribe_root(Arc::new(move |event| {
            let mut state = captured.lock().unwrap();
            state.callbacks += 1;
            for item in event.events {
                let path: Vec<Index> = item.path.iter().map(|(_, i)| i.clone()).collect();
                if let Diff::Graph(diff) = &item.diff {
                    state.graphs.push(diff.as_ref().clone());
                }
                state.value.apply(&path.into(), &[owned(item.diff).into()]);
            }
        }));
        Self {
            state,
            _subscription: subscription,
        }
    }
    fn check(&self, d: &LoroDoc) {
        assert_eq!(self.state.lock().unwrap().value, d.get_deep_value());
    }
}

#[test]
fn collision_batch_emits_complete_stable_identity_records_at_commit() {
    let (d, n, e) = fixture(&["80", "80", "80", "c080"]);
    let g = d.get_graph("g");
    let mirror = Mirror::new(&d);
    let at_base = d.state_frontiers();
    g.reorder_edge(e[3], At::Before(e[1])).unwrap();
    assert_eq!(
        mirror.state.lock().unwrap().callbacks,
        0,
        "partial local edit was published"
    );
    assert_eq!(order(&g, n), vec![e[0], e[3], e[1], e[2]]);
    d.commit();
    mirror.check(&d);
    {
        let observed = mirror.state.lock().unwrap();
        assert_eq!(observed.callbacks, 1);
        assert_eq!(observed.graphs.len(), 1);
        let diff = &observed.graphs[0];
        for id in [e[1], e[2], e[3]] {
            let changed = diff.edges[&id].as_ref().unwrap();
            assert_eq!((changed.id, changed.source, changed.target), (id, n, n));
            assert_eq!(changed.position, g.edge_record(id).unwrap().position);
        }
        assert_eq!(diff.edges.len(), 3);
    }
    let vv = d.oplog_vv();
    let frontiers = d.state_frontiers();
    let callbacks = mirror.state.lock().unwrap().callbacks;
    for _ in 0..3 {
        order(&g, n);
        g.out_edge_at(n, 2);
        g.index_of_out_edge(e[3]);
        json(&d);
        d.export(ExportMode::Snapshot).unwrap();
        d.diff(&at_base, &frontiers).unwrap();
        mirror.check(&d);
    }
    assert_eq!(d.get_pending_txn_len(), 0);
    assert_eq!(d.oplog_vv(), vv);
    assert_eq!(d.state_frontiers(), frontiers);
    assert_eq!(mirror.state.lock().unwrap().callbacks, callbacks);
}

#[test]
fn event_mirror_combines_order_metadata_and_checkout_without_duplicate_text() {
    let (a, n, e) = fixture(&["4080", "80", "c080"]);
    let g = a.get_graph("g");
    let text = g
        .edge_meta(e[1])
        .unwrap()
        .insert_container("body", LoroText::new())
        .unwrap();
    text.insert(0, "base").unwrap();
    g.node_meta(n).unwrap().insert("title", "node").unwrap();
    a.commit();
    let initial = a.state_frontiers();
    let b = copy(&a, 23);
    let mirror_a = Mirror::new(&a);
    let mirror_b = Mirror::new(&b);
    g.reorder_edge(e[1], At::Start).unwrap();
    text.insert(0, "local ").unwrap();
    a.commit();
    mirror_a.check(&a);
    b.get_graph("g")
        .edge_meta(e[1])
        .unwrap()
        .insert("weight", 42)
        .unwrap();
    b.commit();
    mirror_b.check(&b);
    sync(&a, &b);
    mirror_a.check(&a);
    mirror_b.check(&b);
    assert_eq!(text.to_string(), "local base");
    assert_eq!(
        g.edge_meta(e[1])
            .unwrap()
            .get("weight")
            .unwrap()
            .get_deep_value(),
        42.into()
    );
    g.delete_node(n).unwrap();
    a.commit();
    mirror_a.check(&a);
    text.insert(0, "hidden ").unwrap();
    a.commit();
    mirror_a.check(&a);
    g.restore_node(n).unwrap();
    a.commit();
    mirror_a.check(&a);
    assert_eq!(text.to_string(), "hidden local base");
    assert_eq!(order(&g, n), vec![e[1], e[0], e[2]]);
    let latest = a.state_frontiers();
    a.checkout(&initial).unwrap();
    mirror_a.check(&a);
    assert_eq!(order(&g, n), e);
    a.checkout(&latest).unwrap();
    mirror_a.check(&a);
    assert_eq!(text.to_string(), "hidden local base");
    assert_eq!(order(&g, n), vec![e[1], e[0], e[2]]);
}
