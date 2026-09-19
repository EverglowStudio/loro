//! Slow specification: immutable JSON facts -> registers -> visibility -> sort.
//! No production GraphOp, GraphState, fractional-index generator or index is used.

use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Id {
    pub peer: u64,
    pub counter: i32,
}

impl Id {
    pub fn parse(value: &Value) -> Self {
        let (counter, peer) = value.as_str().unwrap().split_once('@').unwrap();
        Self {
            peer: peer.parse().unwrap(),
            counter: counter.parse().unwrap(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    Node(Id),
    Edge {
        id: Id,
        source: Id,
        target: Id,
        position: Vec<u8>,
    },
    Order {
        id: Id,
        position: Vec<u8>,
    },
    DeleteNode(Id),
    DeleteEdge(Id),
    RestoreNode {
        id: Id,
        deletes: BTreeSet<Id>,
    },
    RestoreEdge {
        id: Id,
        deletes: BTreeSet<Id>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fact {
    pub id: Id,
    pub lamport: u64,
    pub graph: String,
    pub kind: Kind,
}

#[derive(Clone, Debug, Default)]
pub struct History(pub BTreeMap<Id, Fact>);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edge {
    pub source: Id,
    pub target: Id,
    pub alive: bool,
    pub visible: bool,
    pub position: Vec<u8>,
    pub writer: Id,
    pub lamport: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub nodes: BTreeMap<Id, bool>,
    pub edges: BTreeMap<Id, Edge>,
}

impl Snapshot {
    pub fn ordered(&self, source: Id) -> Vec<Id> {
        let mut edges: Vec<_> = self
            .edges
            .iter()
            .filter(|(_, e)| e.source == source && e.visible)
            .collect();
        // Numeric identity order, including counter, is independent of LWW.
        edges.sort_by(|(a, x), (b, y)| x.position.cmp(&y.position).then(a.cmp(b)));
        edges.into_iter().map(|(id, _)| *id).collect()
    }
}

pub fn decode_hex(value: &Value) -> Vec<u8> {
    let text = value.as_str().unwrap();
    assert!(!text.is_empty() && text.len() % 2 == 0 && text.is_ascii());
    let bytes: Vec<_> = (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
        .collect();
    assert!(bytes.len() <= 4096 && bytes.last() == Some(&0x80));
    bytes
}

impl History {
    /// Input must be a causally closed boundary, exported without peer compression.
    /// Pending packet closure is computed separately by the three-replica driver.
    pub fn from_json(schema: &Value) -> Self {
        assert!(
            schema["peers"].is_null(),
            "use uncompressed peer identities"
        );
        let mut history = Self::default();
        for change in schema["changes"].as_array().unwrap() {
            let start = Id::parse(&change["id"]);
            for op in change["ops"].as_array().unwrap() {
                let graph = op["container"].as_str().unwrap();
                if !graph.ends_with(":Graph") {
                    continue;
                }
                let content = op["content"].as_object().unwrap();
                assert_eq!(content.len(), 1);
                let (kind, body) = content.iter().next().unwrap();
                let id = Id::parse(&body["id"]);
                let deletes = || {
                    body["deletes"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(Id::parse)
                        .collect()
                };
                let kind = match kind.as_str() {
                    "create_node" => Kind::Node(id),
                    "create_edge" => Kind::Edge {
                        id,
                        source: Id::parse(&body["source"]),
                        target: Id::parse(&body["target"]),
                        position: decode_hex(&body["position"]),
                    },
                    "set_edge_order" => Kind::Order {
                        id,
                        position: decode_hex(&body["position"]),
                    },
                    "delete_node" => Kind::DeleteNode(id),
                    "delete_edge" => Kind::DeleteEdge(id),
                    "restore_node" => Kind::RestoreNode {
                        id,
                        deletes: deletes(),
                    },
                    "restore_edge" => Kind::RestoreEdge {
                        id,
                        deletes: deletes(),
                    },
                    other => panic!("unmodeled graph operation: {other}"),
                };
                let counter = i32::try_from(op["counter"].as_i64().unwrap()).unwrap();
                let offset = u64::try_from(counter - start.counter).unwrap();
                history.insert(Fact {
                    id: Id {
                        peer: start.peer,
                        counter,
                    },
                    lamport: change["lamport"].as_u64().unwrap() + offset,
                    graph: graph.to_owned(),
                    kind,
                });
            }
        }
        history
    }

    pub fn insert(&mut self, fact: Fact) {
        if let Some(previous) = self.0.insert(fact.id, fact.clone()) {
            assert_eq!(
                previous, fact,
                "one operation ID must denote one immutable fact"
            );
        }
    }

    pub fn extend(&mut self, other: &Self) {
        for fact in other.0.values() {
            self.insert(fact.clone());
        }
    }

    pub fn snapshot(&self, graph: &str) -> Snapshot {
        let facts: Vec<_> = self.0.values().filter(|f| f.graph == graph).collect();
        let mut result = Snapshot::default();
        for fact in &facts {
            if let Kind::Node(id) = fact.kind {
                result.nodes.insert(id, alive(id, false, &facts));
            }
        }
        for fact in &facts {
            if let Kind::Edge {
                id, source, target, ..
            } = fact.kind
            {
                // Deliberately rescan every write for every edge. Never select
                // by maximum position, counter, arrival order or record LWW.
                let (winner, position) = facts
                    .iter()
                    .filter_map(|write| match &write.kind {
                        Kind::Edge {
                            id: edge, position, ..
                        }
                        | Kind::Order { id: edge, position }
                            if *edge == id =>
                        {
                            Some((*write, position))
                        }
                        _ => None,
                    })
                    .max_by_key(|(write, _)| (write.lamport, write.id.peer))
                    .unwrap();
                let alive = alive(id, true, &facts);
                result.edges.insert(
                    id,
                    Edge {
                        source,
                        target,
                        alive,
                        visible: alive && result.nodes[&source] && result.nodes[&target],
                        position: position.clone(),
                        writer: winner.id,
                        lamport: winner.lamport,
                    },
                );
            }
        }
        result
    }
}

fn alive(id: Id, edge: bool, facts: &[&Fact]) -> bool {
    // The protocol's observed-remove contract, computed by a full pairwise scan.
    // Undo restores only the listed Delete IDs, not every observed Delete.
    facts.iter().filter(|f| matches!((&f.kind, edge),
        (Kind::DeleteEdge(object), true) | (Kind::DeleteNode(object), false) if *object == id
    )).all(|delete| facts.iter().any(|restore| matches!((&restore.kind, edge),
        (Kind::RestoreEdge { id: object, deletes }, true)
        | (Kind::RestoreNode { id: object, deletes }, false)
            if *object == id && deletes.contains(&delete.id)
    )))
}
