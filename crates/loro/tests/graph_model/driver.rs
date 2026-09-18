//! Real Loro documents, transport packets, and independent causal checkpoints.

use super::{
    native::{graph, Events, Ids, View, GRAPHS},
    oracle::{Action, EdgeId, EventId, Graph, NodeId, Object, Oracle, Snapshot, Version},
};
use loro::{event::Diff, ContainerTrait, ExportMode, Frontiers, LoroDoc, VersionVector, ID};
use rand::{rngs::StdRng, seq::SliceRandom, Rng, SeedableRng};
use std::{
    cell::RefCell,
    collections::{BTreeMap, VecDeque},
    panic::{catch_unwind, AssertUnwindSafe},
};

thread_local! {
    // Each test/fuzzer invocation runs one synchronous driver on its thread.
    // Keeping the trace here also makes it available to libFuzzer's abort hook.
    static TRACE: RefCell<VecDeque<String>> = RefCell::new(VecDeque::new());
}

/// Bounded diagnostics usable before libFuzzer aborts, as well as after a native
/// test unwinds. Avoid a second panic if the original failure held this borrow.
pub fn failure_trace() -> String {
    TRACE.with(|trace| match trace.try_borrow() {
        Ok(trace) => trace.iter().cloned().collect::<Vec<_>>().join("\n"),
        Err(_) => "trace unavailable during trace update".into(),
    })
}

struct Replica {
    doc: LoroDoc,
    /// Received packet identities, including those waiting for dependencies.
    received: Version,
    events: Events,
}

impl Replica {
    fn empty(peer: u64) -> Self {
        let doc = LoroDoc::new();
        doc.set_peer_id(peer).unwrap();
        let events = Events::subscribe(&doc, BTreeMap::new());
        Self {
            doc,
            received: Version::new(),
            events,
        }
    }
}

struct Packet {
    bytes: Vec<u8>,
    peer: u64,
    start: i32,
    end: i32,
}

#[derive(Clone)]
pub struct Cut {
    pub at: Version,
    frontiers: Frontiers,
}

pub struct Run {
    pub oracle: Oracle,
    pub ids: Ids,
    replicas: [Replica; 3],
    packets: Vec<Packet>,
    pub cuts: Vec<Cut>,
    next_peer: u64,
    seed: u64,
    pub step: usize,
}

pub fn with_run(case: &str, seed: u64, f: impl FnOnce(&mut Run)) {
    TRACE.with(|trace| trace.borrow_mut().clear());
    let mut run = Run::new(seed);
    if let Err(panic) = catch_unwind(AssertUnwindSafe(|| f(&mut run))) {
        let reason = panic
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| panic.downcast_ref::<&str>().map(|s| s.to_string()))
            .unwrap_or_else(|| "non-string panic".into());
        panic!("graph differential failed: seed={} step={}\nreplay: LORO_GRAPH_SEED={} LORO_GRAPH_STEPS={} CARGO_INCREMENTAL=0 cargo test -p loro --test graph_model {} -j 2 -- --nocapture\nlast actions:\n{}\noriginal failure: {}",
            run.seed, run.step, run.seed, run.step + 1, case,
            failure_trace(), reason);
    }
}

/// Shared by native tests and the libFuzzer graph target. The caller chooses
/// the step budget; the byte-driven fuzz adapter enforces its own small cap.
pub fn run_seed(seed: u64, steps: usize) {
    with_run("native_random_three_replica_differential", seed, |run| {
        run.bootstrap();
        let mut rng = StdRng::seed_from_u64(seed);
        for step in 0..steps {
            run.step = step;
            run.random_step(&mut rng);
        }
        run.converge();
    });
}

impl Run {
    fn new(seed: u64) -> Self {
        Self {
            oracle: Oracle::default(),
            ids: Ids::default(),
            replicas: [
                Replica::empty(1),
                Replica::empty(2),
                Replica::empty((1 << 54) + 3),
            ],
            packets: Vec::new(),
            cuts: Vec::new(),
            next_peer: 100,
            seed,
            step: 0,
        }
    }

    pub fn doc(&self, replica: usize) -> &LoroDoc {
        &self.replicas[replica].doc
    }

    pub fn note(&mut self, message: impl Into<String>) {
        let line = format!("step {}: {}", self.step, message.into());
        TRACE.with(|trace| {
            let mut trace = trace.borrow_mut();
            if trace.len() == 32 {
                trace.pop_front();
            }
            trace.push_back(line);
        });
    }

    pub fn version(&self, replica: usize) -> Version {
        self.oracle.closed_subset(&self.replicas[replica].received)
    }

    pub fn state(&self, replica: usize, scope: Graph) -> Snapshot {
        self.oracle.snapshot(scope, &self.version(replica))
    }

    fn expected_vv(&self, at: &Version) -> VersionVector {
        let mut vv = VersionVector::default();
        for event in at {
            let packet = &self.packets[event.0];
            vv.extend_to_include_end_id(ID::new(packet.peer, packet.end));
        }
        vv
    }

    fn initial_views(&self, at: &Version) -> BTreeMap<Graph, View> {
        GRAPHS
            .iter()
            .map(|scope| {
                (
                    *scope,
                    self.ids.expected_view(&self.oracle.snapshot(*scope, at)),
                )
            })
            .collect()
    }

    fn check_replica(&self, replica: &Replica, at: &Version) {
        assert!(self.oracle.is_closed(at));
        let vv = self.expected_vv(at);
        assert_eq!(replica.doc.state_vv(), vv, "applied causal checkpoint");
        let before = replica.doc.oplog_vv();
        let frontiers = replica.doc.state_frontiers();
        assert_eq!(replica.doc.get_pending_txn_len(), 0);
        for scope in GRAPHS {
            let expected = self.oracle.snapshot(scope, at);
            self.ids.check(&replica.doc, scope, &expected);
            replica.events.check(scope, &self.ids, &expected);
        }
        // Querying lazily initialized metadata must not secretly add operations
        // or move a historical DocState to the latest OpLog boundary.
        assert_eq!(
            replica.doc.get_pending_txn_len(),
            0,
            "queries wrote operations"
        );
        assert_eq!(replica.doc.oplog_vv(), before, "queries changed OpLog");
        assert_eq!(
            replica.doc.state_frontiers(),
            frontiers,
            "queries changed checkout"
        );
    }

    pub fn check(&self, replica: usize) {
        let at = self.version(replica);
        self.check_replica(&self.replicas[replica], &at);
        assert_eq!(
            self.doc(replica).oplog_vv(),
            self.expected_vv(&at),
            "pending bytes are not applied history"
        );
    }

    pub fn check_all(&self) {
        for replica in 0..3 {
            self.check(replica);
        }
    }

    pub fn act(&mut self, replica: usize, action: Action) -> EventId {
        let at = self.version(replica);
        self.oracle.validate(&at, &action).unwrap();
        let id = self.oracle.next_id();
        self.note(format!("r{replica} {id:?} {action:?}"));
        let doc = &self.replicas[replica].doc;
        let before = doc.oplog_vv();
        let peer = doc.peer_id();
        self.ids.apply(doc, id, &action);
        doc.commit();
        let after = doc.oplog_vv();
        let start = before.get(&peer).copied().unwrap_or(0);
        let end = after.get(&peer).copied().unwrap_or(0);
        assert!(
            end > start,
            "generated action unexpectedly produced no operation"
        );
        let mut local_only = before.clone();
        local_only.set_end(ID::new(peer, end));
        assert_eq!(
            after, local_only,
            "a local edit applied unmodeled operations"
        );
        let bytes = doc.export(ExportMode::updates(&before)).unwrap();
        assert_eq!(self.oracle.append(&at, action), id);
        assert_eq!(self.packets.len(), id.0);
        self.packets.push(Packet {
            bytes,
            peer,
            start,
            end,
        });
        self.replicas[replica].received.insert(id);
        self.check(replica);
        self.save_cut(replica);
        id
    }

    pub fn node(&mut self, replica: usize, scope: Graph) -> NodeId {
        NodeId {
            graph: scope,
            birth: self.act(replica, Action::CreateNode(scope)),
        }
    }

    pub fn edge(&mut self, replica: usize, source: NodeId, target: NodeId) -> EdgeId {
        EdgeId {
            graph: source.graph,
            birth: self.act(
                replica,
                Action::CreateEdge {
                    graph: source.graph,
                    source,
                    target,
                },
            ),
        }
    }

    pub fn deliver(&mut self, to: usize, events: &[EventId], batch: bool) {
        let ranges: Vec<_> = events
            .iter()
            .map(|id| {
                let p = &self.packets[id.0];
                (id.0, p.peer, p.start, p.end)
            })
            .collect();
        self.note(format!(
            "deliver ->r{to} batch={batch} (event,peer,start,end)={ranges:?}"
        ));
        if batch {
            let blobs: Vec<_> = events
                .iter()
                .map(|id| self.packets[id.0].bytes.as_slice())
                .collect();
            self.doc(to).import_updates_batch(&blobs).unwrap();
            self.replicas[to].received.extend(events.iter().copied());
            self.check(to);
        } else {
            for event in events {
                self.doc(to).import(&self.packets[event.0].bytes).unwrap();
                self.replicas[to].received.insert(*event);
                self.check(to);
            }
        }
        self.save_cut(to);
    }

    pub fn send_all(&mut self, from: usize, to: usize) {
        let events: Vec<_> = self.version(from).into_iter().collect();
        self.note(format!("sync r{from}->r{to}"));
        self.deliver(to, &events, true);
    }

    pub fn save_cut(&mut self, replica: usize) -> usize {
        let cut = Cut {
            at: self.version(replica),
            frontiers: self.doc(replica).state_frontiers(),
        };
        self.cuts.push(cut);
        self.cuts.len() - 1
    }

    pub fn checkout_roundtrip(&mut self, replica: usize, cut: usize) {
        let target = self.cuts[cut].clone();
        let latest = Cut {
            at: self.version(replica),
            frontiers: self.doc(replica).state_frontiers(),
        };
        assert!(target.at.is_subset(&latest.at));
        self.note(format!(
            "checkout r{replica} cut={cut} {:?}",
            target.frontiers
        ));
        self.check_diff(replica, &latest, &target);
        self.check_diff(replica, &target, &latest);
        self.doc(replica).checkout(&target.frontiers).unwrap();
        self.check_replica(&self.replicas[replica], &target.at);
        self.doc(replica).checkout_to_latest();
        self.check(replica);
    }

    fn check_diff(&self, replica: usize, from: &Cut, to: &Cut) {
        let doc = self.doc(replica);
        let before = doc.state_frontiers();
        let diff = doc.diff(&from.frontiers, &to.frontiers).unwrap();
        let mut reconstructed = self.initial_views(&from.at);
        for (container, delta) in diff.iter() {
            if let Diff::Graph(records) = delta {
                let scope = GRAPHS
                    .iter()
                    .copied()
                    .find(|scope| graph(doc, *scope).id() == *container)
                    .expect("unexpected graph in historical diff");
                reconstructed.entry(scope).or_default().apply(records);
            }
        }
        assert_eq!(
            reconstructed,
            self.initial_views(&to.at),
            "historical diff reconstruction"
        );
        assert_eq!(
            doc.state_frontiers(),
            before,
            "diff changed selected history"
        );
    }

    pub fn fork_editor(&mut self, from: usize, to: usize, cut: usize) {
        let target = self.cuts[cut].clone();
        assert!(target.at.is_subset(&self.version(from)));
        self.note(format!(
            "fork r{from} cut={cut}->r{to} new peer={}",
            self.next_peer
        ));
        let doc = self.doc(from).fork_at(&target.frontiers).unwrap();
        doc.set_peer_id(self.next_peer).unwrap();
        self.next_peer += 1;
        // Forking creates an already populated document, so subscribe from the
        // independently known starting view, not a production query or diff.
        let events = Events::subscribe(&doc, self.initial_views(&target.at));
        self.replicas[to] = Replica {
            doc,
            received: target.at,
            events,
        };
        self.check(to);
    }

    pub fn snapshot_roundtrip(&mut self, from: usize) {
        self.note(format!("snapshot/rebuild r{from}"));
        let at = self.version(from);
        let bytes = self.doc(from).export(ExportMode::Snapshot).unwrap();
        let mut rebuilt = Replica::empty(self.next_peer);
        self.next_peer += 1;
        rebuilt.doc.import(&bytes).unwrap();
        rebuilt.received = at.clone();
        self.check_replica(&rebuilt, &at);
        assert_eq!(rebuilt.doc.oplog_vv(), self.expected_vv(&at));
    }

    pub fn merge_snapshot(&mut self, from: usize, to: usize) {
        self.note(format!("snapshot merge r{from}->r{to}"));
        let at = self.version(from);
        let bytes = self.doc(from).export(ExportMode::Snapshot).unwrap();
        self.doc(to).import(&bytes).unwrap();
        self.replicas[to].received.extend(at);
        self.check(to);
        self.save_cut(to);
    }

    pub fn converge(&mut self) {
        self.note("reconnect every replica, including packets from replaced forks");
        // A separate RNG means shrinking the random operation prefix does not
        // change any generation decisions before that prefix.
        let mut rng = StdRng::seed_from_u64(self.seed ^ 0xF011_DE11);
        for replica in 0..3 {
            let mut events: Vec<_> = (0..self.packets.len()).map(EventId).collect();
            events.shuffle(&mut rng);
            let mut offset = 0;
            while offset < events.len() {
                let end = (offset + rng.gen_range(1..=4)).min(events.len());
                let mut chunk = events[offset..end].to_vec();
                if rng.gen_bool(0.5) {
                    chunk.push(chunk[0]);
                }
                self.deliver(replica, &chunk, rng.gen_bool(0.5));
                offset = end;
            }
            assert_eq!(self.version(replica).len(), self.oracle.events.len());
        }
        self.check_all();
        for replica in 0..3 {
            self.snapshot_roundtrip(replica);
        }
    }

    /// Seed graph shapes guarantee that random runs start with more than trees:
    /// diamond, overlapping cycles, parallel edges, self-loop, disconnected data.
    pub fn bootstrap(&mut self) {
        let nodes: Vec<_> = (0..7).map(|_| self.node(0, Graph(0))).collect();
        for (source, target) in [
            (0, 1),
            (0, 2),
            (1, 3),
            (2, 3),
            (3, 0),
            (0, 0),
            (0, 1),
            (3, 1),
            (5, 6),
        ] {
            self.edge(0, nodes[source], nodes[target]);
        }
        let other_a = self.node(0, Graph(1));
        let other_b = self.node(0, Graph(1));
        self.edge(0, other_a, other_b);
        self.send_all(0, 1);
        let reverse: Vec<_> = self.version(0).into_iter().rev().collect();
        self.deliver(2, &reverse, false);
        self.check_all();
    }

    pub fn random_step(&mut self, rng: &mut StdRng) {
        let replica = rng.gen_range(0..3);
        let scope = GRAPHS[rng.gen_range(0..GRAPHS.len())];
        let state = self.state(replica, scope);
        let nodes: Vec<_> = state.nodes.keys().copied().collect();
        let visible: Vec<_> = state.visible_nodes().into_iter().collect();
        let objects: Vec<_> = nodes
            .iter()
            .copied()
            .map(Object::Node)
            .chain(state.edges.keys().copied().map(Object::Edge))
            .collect();
        match rng.gen_range(0..16) {
            0 => {
                self.node(replica, scope);
            }
            1 | 2 if !visible.is_empty() => {
                self.edge(
                    replica,
                    *visible.choose(rng).unwrap(),
                    *visible.choose(rng).unwrap(),
                );
            }
            3..=5 if !objects.is_empty() => {
                let object = *objects.choose(rng).unwrap();
                let alive = match object {
                    Object::Node(n) => state.nodes[&n].alive,
                    Object::Edge(e) => state.edges[&e].alive,
                };
                self.act(
                    replica,
                    if alive {
                        Action::Delete(object)
                    } else {
                        Action::Restore(object)
                    },
                );
            }
            6 | 7 if !objects.is_empty() => {
                let object = *objects.choose(rng).unwrap();
                let key = format!("writer/{}", self.doc(replica).peer_id());
                let action =
                    if rng.gen_bool(0.3) && state.properties(object).scalars.contains_key(&key) {
                        Action::Remove { object, key }
                    } else {
                        Action::Set {
                            object,
                            key,
                            value: format!("seed/{}/event/{}", self.seed, self.oracle.events.len()),
                        }
                    };
                self.act(replica, action);
            }
            8..=11 => {
                let to = (replica + rng.gen_range(1..=2)) % 3;
                let mut events: Vec<_> = self.version(replica).into_iter().collect();
                events.shuffle(rng);
                events.truncate(rng.gen_range(1..=4));
                if !events.is_empty() && rng.gen_bool(0.5) {
                    events.push(events[0]);
                }
                self.note(format!("partial r{replica}->r{to}"));
                self.deliver(to, &events, rng.gen_bool(0.5));
            }
            12 | 13 => {
                let known = self.version(replica);
                let valid: Vec<_> = self
                    .cuts
                    .iter()
                    .enumerate()
                    .filter_map(|(i, c)| c.at.is_subset(&known).then_some(i))
                    .collect();
                if let Some(&cut) = valid.choose(rng) {
                    self.checkout_roundtrip(replica, cut);
                }
            }
            14 => {
                let known = self.version(replica);
                let valid: Vec<_> = self
                    .cuts
                    .iter()
                    .enumerate()
                    .filter_map(|(i, c)| c.at.is_subset(&known).then_some(i))
                    .collect();
                if let Some(&cut) = valid.choose(rng) {
                    let to = (replica + 1) % 3;
                    self.fork_editor(replica, to, cut);
                    // Historical editing is exercised even if the subsequent
                    // random choices happen to contain only network traffic.
                    self.node(to, scope);
                }
            }
            15 => self.merge_snapshot(replica, (replica + 1) % 3),
            _ => self.snapshot_roundtrip(replica),
        }
    }
}
