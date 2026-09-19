//! Transport closure is test-owned. Pending packets never become expected state
//! just because the production document says they were applied.
use super::{
    oracle::{History, Snapshot},
    support::*,
};
use loro::{ContainerTrait, ExportMode, Frontiers, LoroDoc, VersionVector, ID};
use std::collections::BTreeSet;

struct Packet {
    bytes: Vec<u8>,
    facts: History,
    ancestors: BTreeSet<usize>,
    vv: VersionVector,
}
struct Cut {
    frontiers: Frontiers,
    facts: History,
}

pub const SCOPES: [&str; 2] = ["g", "other"];
pub struct Run {
    pub docs: [LoroDoc; 3],
    packets: Vec<Packet>,
    received: [BTreeSet<usize>; 3],
    cuts: [Vec<Cut>; 3],
    pub trace: Vec<String>,
}
impl Run {
    pub fn new() -> Self {
        let (base, _, _) = fixture(&["80", "80", "80", "c080"]);
        let other = base.get_graph("other");
        let p = other.create_node().unwrap();
        let q = other.create_node().unwrap();
        other.create_edge(p, q).unwrap();
        other.create_edge(p, p).unwrap();
        other.create_edge(q, p).unwrap();
        base.commit();
        let packet = Packet {
            bytes: base.export(ExportMode::all_updates()).unwrap(),
            facts: history(&base),
            ancestors: BTreeSet::new(),
            vv: base.oplog_vv(),
        };
        let mut run = Self {
            docs: [doc(2), doc(3), doc((1 << 54) + 4)],
            packets: vec![packet],
            received: Default::default(),
            cuts: Default::default(),
            trace: vec![],
        };
        for r in 0..3 {
            run.deliver(r, 0);
            run.save_cut(r);
        }
        run
    }

    fn applied(&self, r: usize) -> BTreeSet<usize> {
        self.received[r]
            .iter()
            .copied()
            .filter(|i| self.packets[*i].ancestors.is_subset(&self.received[r]))
            .collect()
    }
    pub fn expected(&self, r: usize) -> History {
        let mut history = History::default();
        for i in self.applied(r) {
            history.extend(&self.packets[i].facts);
        }
        history
    }
    pub fn state(&self, r: usize, scope: &str) -> Snapshot {
        self.expected(r)
            .snapshot(&self.docs[r].get_graph(scope).id().to_string())
    }
    pub fn check(&self, r: usize) {
        let mut vv = VersionVector::default();
        for packet in self.applied(r) {
            for (peer, end) in self.packets[packet].vv.iter() {
                if *end > vv.get(peer).copied().unwrap_or(0) {
                    vv.set_end(ID::new(*peer, *end));
                }
            }
        }
        assert_eq!(
            self.docs[r].oplog_vv(),
            vv,
            "replica {r}: transport causal closure"
        );
        assert_eq!(
            self.docs[r].state_vv(),
            vv,
            "replica {r}: selected state version"
        );
        let frontiers = self.docs[r].state_frontiers();
        let expected = self.expected(r);
        for scope in SCOPES {
            check(&self.docs[r], scope, &expected);
        }
        assert_eq!(self.docs[r].get_pending_txn_len(), 0);
        assert_eq!(self.docs[r].oplog_vv(), vv);
        assert_eq!(self.docs[r].state_frontiers(), frontiers);
    }
    pub fn note(&mut self, line: impl Into<String>) {
        self.trace.push(line.into());
    }

    pub fn record(&mut self, r: usize, before: VersionVector) {
        let ancestors = self.applied(r);
        self.docs[r].commit();
        let after = self.docs[r].oplog_vv();
        if before != after {
            let peer = self.docs[r].peer_id();
            let end = *after.get(&peer).unwrap();
            assert!(end > before.get(&peer).copied().unwrap_or(0));
            let mut local_only = before.clone();
            local_only.set_end(ID::new(peer, end));
            assert_eq!(
                local_only, after,
                "local action unexpectedly applied pending remote operations"
            );
            self.packets.push(Packet {
                bytes: self.docs[r].export(ExportMode::updates(&before)).unwrap(),
                facts: History::from_json(&json_between(&self.docs[r], &before, &after)),
                ancestors,
                vv: after,
            });
            self.received[r].insert(self.packets.len() - 1);
        }
        self.check(r);
        self.save_cut(r);
    }

    pub fn packet_count(&self) -> usize {
        self.packets.len()
    }
    pub fn deliver(&mut self, to: usize, packet: usize) {
        self.note(format!("deliver packet {packet} -> r{to}"));
        self.docs[to].import(&self.packets[packet].bytes).unwrap();
        self.received[to].insert(packet);
        self.check(to);
    }
    pub fn sync(&mut self, from: usize, to: usize) {
        for packet in self.applied(from) {
            self.deliver(to, packet);
        }
    }
    pub fn converge(&mut self) {
        for r in 0..3 {
            for packet in 0..self.packets.len() {
                if !self.received[r].contains(&packet) {
                    self.deliver(r, packet);
                }
            }
            self.check(r);
            assert_eq!(self.docs[r].get_deep_value(), self.docs[0].get_deep_value());
        }
    }
    fn save_cut(&mut self, r: usize) {
        let facts = self.expected(r);
        self.cuts[r].push(Cut {
            frontiers: self.docs[r].state_frontiers(),
            facts,
        });
    }
    pub fn reload(&mut self, r: usize) {
        // Reload only complete received histories: do not silently discard pending packets.
        if self.received[r] != self.applied(r) {
            return;
        }
        self.note(format!("snapshot reload r{r}"));
        self.docs[r] = copy(&self.docs[r], self.docs[r].peer_id());
        self.check(r);
    }
    pub fn checkout(&mut self, r: usize, choice: usize) {
        let cut_index = choice % self.cuts[r].len();
        self.note(format!("checkout roundtrip r{r} cut {cut_index}"));
        let cut = &self.cuts[r][cut_index];
        let latest = self.docs[r].state_frontiers();
        let vv = self.docs[r].oplog_vv();
        self.docs[r].checkout(&cut.frontiers).unwrap();
        for scope in SCOPES {
            check(&self.docs[r], scope, &cut.facts);
        }
        assert_eq!(self.docs[r].state_frontiers(), cut.frontiers);
        assert_eq!(self.docs[r].oplog_vv(), vv);
        self.docs[r].checkout(&latest).unwrap();
        self.docs[r].checkout_to_latest();
        self.check(r);
    }
}
