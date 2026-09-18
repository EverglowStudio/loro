//! Public API translation only. Expected state always comes from oracle.rs.

use super::oracle::{self, Action, EdgeId, EventId, Graph, NodeId, Object, Properties, Snapshot};
use loro::{
    event::Diff, Container, ContainerID, ContainerTrait, GraphDiff, GraphEdgeId, GraphNodeId,
    LoroDoc, LoroGraph, LoroMap, LoroText, Subscription, ToJson, ValueOrContainer,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Debug,
    sync::{Arc, Mutex},
};

pub const GRAPHS: [Graph; 2] = [Graph(0), Graph(1)];

pub fn graph(doc: &LoroDoc, scope: Graph) -> LoroGraph {
    doc.get_graph(match scope {
        Graph(0) => "graph-model/main",
        Graph(1) => "graph-model/other",
        _ => panic!("unexpected graph scope"),
    })
}

#[derive(Default)]
pub struct Ids {
    pub nodes: BTreeMap<NodeId, GraphNodeId>,
    pub edges: BTreeMap<EdgeId, GraphEdgeId>,
    metadata: BTreeMap<Object, ContainerID>,
    texts: BTreeMap<(Object, String), ContainerID>,
}

impl Ids {
    pub fn meta(&self, doc: &LoroDoc, object: Object) -> LoroMap {
        let g = graph(doc, object.graph());
        match object {
            Object::Node(id) => g.node_meta(self.nodes[&id]).unwrap(),
            Object::Edge(id) => g.edge_meta(self.edges[&id]).unwrap(),
        }
    }

    pub fn apply(&mut self, doc: &LoroDoc, id: EventId, action: &Action) {
        let object = action.object(id);
        let g = graph(doc, object.graph());
        match action {
            Action::CreateNode(scope) => {
                let actual = g.create_node().unwrap();
                assert!(self.nodes.values().all(|other| *other != actual));
                assert!(self.edges.values().all(|other| other.id() != actual.id()));
                assert!(self
                    .nodes
                    .insert(
                        NodeId {
                            graph: *scope,
                            birth: id
                        },
                        actual
                    )
                    .is_none());
                self.remember_meta(object, g.node_meta(actual).unwrap().id());
            }
            Action::CreateEdge {
                graph,
                source,
                target,
            } => {
                let actual = g
                    .create_edge(self.nodes[source], self.nodes[target])
                    .unwrap();
                assert!(self.edges.values().all(|other| *other != actual));
                assert!(self.nodes.values().all(|other| other.id() != actual.id()));
                assert!(self
                    .edges
                    .insert(
                        EdgeId {
                            graph: *graph,
                            birth: id
                        },
                        actual
                    )
                    .is_none());
                self.remember_meta(object, g.edge_meta(actual).unwrap().id());
            }
            Action::Delete(Object::Node(id)) => g.delete_node(self.nodes[id]).unwrap(),
            Action::Delete(Object::Edge(id)) => g.delete_edge(self.edges[id]).unwrap(),
            Action::Restore(Object::Node(id)) => g.restore_node(self.nodes[id]).unwrap(),
            Action::Restore(Object::Edge(id)) => g.restore_edge(self.edges[id]).unwrap(),
            Action::Set { object, key, value } => {
                self.meta(doc, *object).insert(key, value.as_str()).unwrap()
            }
            Action::Remove { object, key } => self.meta(doc, *object).delete(key).unwrap(),
            Action::SeedText { object, key, value } => {
                let text = self
                    .meta(doc, *object)
                    .insert_container(key, LoroText::new())
                    .unwrap();
                text.insert(0, value).unwrap();
                assert!(self
                    .texts
                    .insert((*object, key.clone()), text.id())
                    .is_none());
            }
            Action::PrefixText { object, key, value } => {
                self.text(doc, *object, key).insert(0, value).unwrap();
            }
            Action::SuffixText { object, key, value } => {
                let text = self.text(doc, *object, key);
                text.insert(text.len_unicode(), value).unwrap();
            }
        }
    }

    fn remember_meta(&mut self, object: Object, id: ContainerID) {
        assert!(
            self.metadata.values().all(|other| *other != id),
            "graph objects share a metadata container"
        );
        assert!(self.metadata.insert(object, id).is_none());
    }

    pub fn text(&self, doc: &LoroDoc, object: Object, key: &str) -> LoroText {
        match self.meta(doc, object).get(key).unwrap() {
            ValueOrContainer::Container(Container::Text(text)) => text,
            other => panic!("expected nested Text, got {other:?}"),
        }
    }

    pub fn expected_view(&self, expected: &Snapshot) -> View {
        View {
            nodes: expected
                .nodes
                .iter()
                .map(|(id, n)| (self.nodes[id], (n.alive, n.alive)))
                .collect(),
            edges: expected
                .edges
                .iter()
                .map(|(id, e)| {
                    (
                        self.edges[id],
                        EdgeView {
                            source: self.nodes[&e.source],
                            target: self.nodes[&e.target],
                            alive: e.alive,
                            visible: e.visible,
                        },
                    )
                })
                .collect(),
        }
    }

    pub fn check(&self, doc: &LoroDoc, scope: Graph, expected: &Snapshot) {
        let g = graph(doc, scope);
        let node_records = g.node_records();
        let edge_records = g.edge_records();
        assert_ids(
            node_records.iter().map(|n| n.id).collect(),
            expected.nodes.keys().map(|id| self.nodes[id]).collect(),
            "all node records",
        );
        assert_ids(
            edge_records.iter().map(|e| e.id).collect(),
            expected.edges.keys().map(|id| self.edges[id]).collect(),
            "all edge records",
        );
        let actual = View {
            nodes: node_records
                .into_iter()
                .map(|n| (n.id, (n.alive, n.visible)))
                .collect(),
            edges: edge_records
                .into_iter()
                .map(|e| {
                    (
                        e.id,
                        EdgeView {
                            source: e.source,
                            target: e.target,
                            alive: e.alive,
                            visible: e.visible,
                        },
                    )
                })
                .collect(),
        };
        assert_eq!(
            actual,
            self.expected_view(expected),
            "record lifecycle/endpoints in {scope:?}"
        );
        assert_ids(
            g.nodes(),
            expected
                .visible_nodes()
                .iter()
                .map(|id| self.nodes[id])
                .collect(),
            "visible nodes",
        );
        assert_ids(
            g.edges(),
            expected
                .visible_edges()
                .iter()
                .map(|id| self.edges[id])
                .collect(),
            "visible edges",
        );
        assert_eq!(g.node_count(), expected.visible_nodes().len(), "node count");
        assert_eq!(g.edge_count(), expected.visible_edges().len(), "edge count");

        // Include IDs that are known globally but absent at this replica or
        // history boundary. A record from a future branch must not leak in.
        for (id, actual) in self.nodes.iter().filter(|(id, _)| id.graph == scope) {
            let record = g.node_record(*actual);
            assert_eq!(
                record.is_some(),
                expected.nodes.contains_key(id),
                "node record {id:?}"
            );
            let Some(n) = expected.nodes.get(id) else {
                assert!(g.get_node(*actual).is_none());
                continue;
            };
            let record = record.unwrap();
            assert_eq!(
                (record.id, record.alive, record.visible),
                (*actual, n.alive, n.alive)
            );
            assert_eq!(g.get_node(*actual).is_some(), n.alive);
            assert_ids(
                g.incoming_edges(*actual).unwrap(),
                expected
                    .incoming(*id)
                    .iter()
                    .map(|e| self.edges[e])
                    .collect(),
                "incoming edges",
            );
            assert_ids(
                g.outgoing_edges(*actual).unwrap(),
                expected
                    .outgoing(*id)
                    .iter()
                    .map(|e| self.edges[e])
                    .collect(),
                "outgoing edges",
            );
            assert_ids(
                g.predecessors(*actual).unwrap(),
                expected
                    .predecessors(*id)
                    .iter()
                    .map(|n| self.nodes[n])
                    .collect(),
                "predecessors",
            );
            assert_ids(
                g.successors(*actual).unwrap(),
                expected
                    .successors(*id)
                    .iter()
                    .map(|n| self.nodes[n])
                    .collect(),
                "successors",
            );
            self.check_properties(doc, Object::Node(*id), &n.properties);
        }
        for (id, actual) in self.edges.iter().filter(|(id, _)| id.graph == scope) {
            let record = g.edge_record(*actual);
            assert_eq!(
                record.is_some(),
                expected.edges.contains_key(id),
                "edge record {id:?}"
            );
            let Some(e) = expected.edges.get(id) else {
                assert!(g.get_edge(*actual).is_none());
                continue;
            };
            let record = record.unwrap();
            assert_eq!(
                (
                    record.id,
                    record.source,
                    record.target,
                    record.alive,
                    record.visible
                ),
                (
                    *actual,
                    self.nodes[&e.source],
                    self.nodes[&e.target],
                    e.alive,
                    e.visible
                )
            );
            assert_eq!(g.get_edge(*actual).is_some(), e.visible);
            self.check_properties(doc, Object::Edge(*id), &e.properties);
        }
    }

    fn check_properties(&self, doc: &LoroDoc, object: Object, expected: &Properties) {
        let meta = self.meta(doc, object);
        assert_eq!(
            meta.id(),
            self.metadata[&object],
            "metadata identity across replicas/history"
        );
        let expected_json: serde_json::Map<_, _> = expected
            .scalars
            .iter()
            .chain(&expected.texts)
            .map(|(key, value)| (key.clone(), serde_json::Value::String(value.clone())))
            .collect();
        assert_eq!(
            meta.get_deep_value().to_json_value(),
            serde_json::Value::Object(expected_json),
            "properties of {object:?}"
        );
        for key in expected.scalars.keys() {
            assert!(
                matches!(meta.get(key), Some(ValueOrContainer::Value(_))),
                "scalar became a container"
            );
        }
        for (key, value) in &expected.texts {
            let text = self.text(doc, object, key);
            assert_eq!(
                text.id(),
                self.texts[&(object, key.clone())],
                "nested Text identity"
            );
            assert_eq!(&text.to_string(), value, "nested Text content");
        }
    }
}

fn assert_ids<T: Copy + Ord + Debug>(actual: Vec<T>, expected: BTreeSet<T>, name: &str) {
    let set: BTreeSet<_> = actual.iter().copied().collect();
    assert_eq!(actual.len(), set.len(), "duplicate IDs in {name}");
    assert_eq!(set, expected, "{name}");
    assert!(
        actual.windows(2).all(|pair| pair[0] < pair[1]),
        "{name} must have stable ID order"
    );
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EdgeView {
    source: GraphNodeId,
    target: GraphNodeId,
    alive: bool,
    visible: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct View {
    nodes: BTreeMap<GraphNodeId, (bool, bool)>,
    edges: BTreeMap<GraphEdgeId, EdgeView>,
}

impl View {
    /// Assign public record payloads, ignoring operation/tag internals entirely.
    pub fn apply(&mut self, diff: &GraphDiff) {
        for (id, record) in &diff.nodes {
            match record {
                Some(n) => {
                    assert_eq!(*id, n.id);
                    self.nodes.insert(*id, (n.alive, n.visible));
                }
                None => {
                    self.nodes.remove(id);
                }
            }
        }
        for (id, record) in &diff.edges {
            match record {
                Some(e) => {
                    assert_eq!(*id, e.id);
                    self.edges.insert(
                        *id,
                        EdgeView {
                            source: e.source,
                            target: e.target,
                            alive: e.alive,
                            visible: e.visible,
                        },
                    );
                }
                None => {
                    self.edges.remove(id);
                }
            }
        }
    }
}

/// The subscriber only assigns/removes public event payloads. In particular it
/// must NOT recalculate visibility from endpoints: doing so would conceal a
/// missing derived edge event when a node disappears or reappears.
pub struct Events {
    views: Arc<Mutex<BTreeMap<Graph, View>>>,
    _subscription: Subscription,
}

impl Events {
    pub fn subscribe(doc: &LoroDoc, initial: BTreeMap<Graph, View>) -> Self {
        let views = Arc::new(Mutex::new(initial));
        let captured = views.clone();
        let scopes: Vec<_> = GRAPHS
            .iter()
            .map(|scope| (*scope, graph(doc, *scope).id()))
            .collect();
        let subscription = doc.subscribe_root(Arc::new(move |event| {
            for item in event.events {
                if let Diff::Graph(diff) = item.diff {
                    let scope = scopes
                        .iter()
                        .find(|(_, id)| id == item.target)
                        .expect("unexpected graph event")
                        .0;
                    let mut views = captured.lock().unwrap();
                    let view = views.entry(scope).or_default();
                    view.apply(&diff);
                }
            }
        }));
        Self {
            views,
            _subscription: subscription,
        }
    }

    pub fn check(&self, scope: Graph, ids: &Ids, expected: &oracle::Snapshot) {
        let actual = self
            .views
            .lock()
            .unwrap()
            .get(&scope)
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            actual,
            ids.expected_view(expected),
            "event reconstruction in {scope:?}"
        );
    }
}
