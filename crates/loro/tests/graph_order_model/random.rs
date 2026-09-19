//! Fixed-seed replay: LORO_GRAPH_ORDER_SEED=<u64> LORO_GRAPH_ORDER_STEPS=<prefix>.
//! Every reorder/create first checks the local list intent independently of keys;
//! every edit/import then checks the operation-set oracle, including hidden writes.
use super::{
    driver::{Run, SCOPES},
    oracle::Id,
    support::*,
};
use loro::GraphOrderTarget as At;
use rand::{rngs::StdRng, Rng, SeedableRng};
use std::panic::{catch_unwind, AssertUnwindSafe};

fn destination(rng: &mut StdRng, edges: &[Id]) -> At {
    match rng.gen_range(0..4) {
        0 => At::Start,
        1 => At::End,
        2 if !edges.is_empty() => At::Before(edge(edges[rng.gen_range(0..edges.len())])),
        _ if !edges.is_empty() => At::After(edge(edges[rng.gen_range(0..edges.len())])),
        _ => At::End,
    }
}
fn gap(edges: &[Id], at: At) -> usize {
    match at {
        At::Start => 0,
        At::End => edges.len(),
        At::Before(anchor) => edges.iter().position(|id| *id == eid(anchor)).unwrap(),
        At::After(anchor) => edges.iter().position(|id| *id == eid(anchor)).unwrap() + 1,
    }
}

impl Run {
    pub(super) fn random_step(&mut self, rng: &mut StdRng, step: usize) {
        let r = rng.gen_range(0..3);
        let action = rng.gen_range(0..11);
        let scope = SCOPES[rng.gen_range(0..SCOPES.len())];
        self.note(format!("step {step}: r{r} {scope} action {action}"));
        if action == 6 {
            self.deliver(r, rng.gen_range(0..self.packet_count()));
            return;
        }
        if action == 7 {
            self.sync(r, (r + 1) % 3);
            return;
        }
        if action == 8 {
            self.reload(r);
            return;
        }
        if action == 9 {
            self.checkout(r, rng.gen_range(0..1000));
            return;
        }
        let state = self.state(r, scope);
        let g = self.docs[r].get_graph(scope);
        let before = self.docs[r].oplog_vv();
        let nodes: Vec<_> = state.nodes.keys().copied().collect();
        let edges: Vec<_> = state.edges.keys().copied().collect();
        match action {
            0 => {
                g.create_node().unwrap();
            }
            1 | 10 if !nodes.is_empty() => {
                let source = nodes[rng.gen_range(0..nodes.len())];
                let target = nodes[rng.gen_range(0..nodes.len())];
                let mut intended = state.ordered(source);
                let at = destination(rng, &intended);
                self.note(format!("create {source:?}->{target:?} {at:?}"));
                let created = g.create_edge_at(node(source), node(target), at).unwrap();
                if state.nodes[&source] && state.nodes[&target] {
                    let index = gap(&intended, at);
                    intended.insert(index, eid(created));
                }
                assert_eq!(
                    order(&g, node(source)),
                    intended.into_iter().map(edge).collect::<Vec<_>>()
                );
            }
            2 => {
                let visible: Vec<_> = state
                    .edges
                    .iter()
                    .filter(|(_, e)| e.visible)
                    .map(|(id, _)| *id)
                    .collect();
                if visible.is_empty() {
                    return;
                }
                let moved = visible[rng.gen_range(0..visible.len())];
                let source = state.edges[&moved].source;
                let old = state.ordered(source);
                let at = destination(rng, &old);
                let mut intended = old.clone();
                if !matches!(at, At::Before(id) | At::After(id) if eid(id) == moved) {
                    intended.retain(|id| *id != moved);
                    let index = gap(&intended, at);
                    intended.insert(index, moved);
                }
                self.note(format!("reorder {moved:?} {at:?}"));
                let outcome = g.reorder_edge(edge(moved), at).unwrap();
                assert_eq!(outcome.changed, intended != old, "no-op classification");
                assert_eq!(
                    order(&g, node(source)),
                    intended.into_iter().map(edge).collect::<Vec<_>>()
                );
                if !outcome.changed {
                    assert_eq!(self.docs[r].get_pending_txn_len(), 0);
                    assert_eq!(self.docs[r].oplog_vv(), before);
                }
            }
            3 if !nodes.is_empty() => {
                let id = nodes[rng.gen_range(0..nodes.len())];
                self.note(format!("toggle node {id:?}"));
                if state.nodes[&id] {
                    g.delete_node(node(id)).unwrap();
                } else {
                    g.restore_node(node(id)).unwrap();
                }
            }
            4 if !edges.is_empty() => {
                let id = edges[rng.gen_range(0..edges.len())];
                self.note(format!("toggle edge {id:?}"));
                if state.edges[&id].alive {
                    g.delete_edge(edge(id)).unwrap();
                } else {
                    g.restore_edge(edge(id)).unwrap();
                }
            }
            5 if !nodes.is_empty() => {
                let id = nodes[rng.gen_range(0..nodes.len())];
                let key = format!("writer/{r}");
                g.node_meta(node(id))
                    .unwrap()
                    .insert(&key, step as i64)
                    .unwrap();
                assert_eq!(
                    g.node_meta(node(id))
                        .unwrap()
                        .get(&key)
                        .unwrap()
                        .get_deep_value(),
                    (step as i64).into()
                );
            }
            _ => return,
        }
        self.record(r, before);
    }
}

#[test]
fn fixed_seed_three_replica_ordering_matches_independent_history() {
    let seeds = std::env::var("LORO_GRAPH_ORDER_SEED")
        .ok()
        .map(|s| vec![s.parse::<u64>().unwrap()])
        .unwrap_or_else(|| vec![0, 1, 42, 61516, 0xC0111510, 0x1_0000_0001]);
    let steps = std::env::var("LORO_GRAPH_ORDER_STEPS")
        .ok()
        .map(|s| s.parse().unwrap())
        .unwrap_or(120);
    for seed in seeds {
        let mut run = Run::new();
        let mut rng = StdRng::seed_from_u64(seed);
        let mut failed_step = 0;
        let result = catch_unwind(AssertUnwindSafe(|| {
            for step in 0..steps {
                failed_step = step;
                run.random_step(&mut rng, step);
            }
            run.converge();
        }));
        if let Err(error) = result {
            let reason = error
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| error.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_default();
            panic!("ordering model seed={seed}, step={failed_step}: {reason}\nReplay/minimize prefix: LORO_GRAPH_ORDER_SEED={seed} LORO_GRAPH_ORDER_STEPS={} cargo test -p loro --test graph_order fixed_seed_three_replica_ordering_matches_independent_history -j 2 -- --nocapture\n{}",
                failed_step + 1, run.trace.join("\n"));
        }
    }
}
