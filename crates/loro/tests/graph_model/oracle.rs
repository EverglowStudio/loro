//! Deliberately slow operation-set specification. No Loro types belong here.

use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Graph(pub u8);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct EventId(pub usize);

pub type Version = BTreeSet<EventId>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct NodeId {
    pub graph: Graph,
    pub birth: EventId,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct EdgeId {
    pub graph: Graph,
    pub birth: EventId,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Object {
    Node(NodeId),
    Edge(EdgeId),
}

impl Object {
    pub fn graph(self) -> Graph {
        match self {
            Self::Node(id) => id.graph,
            Self::Edge(id) => id.graph,
        }
    }

    pub fn birth(self) -> EventId {
        match self {
            Self::Node(id) => id.birth,
            Self::Edge(id) => id.birth,
        }
    }
}

/// Scalar keys have one writer in the generated histories. Text cases use
/// disjoint prefix/suffix insertions around a shared, nonempty seed string.
/// This checks graph/property composition without reimplementing Loro's LWW
/// registers or rich-text sequence CRDT in the graph oracle.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    CreateNode(Graph),
    CreateEdge {
        graph: Graph,
        source: NodeId,
        target: NodeId,
    },
    Delete(Object),
    /// Ordinary explicit restore observes all active deletes in its causal past.
    /// Selective Undo is not this action: it may keep an observed remote Delete.
    Restore(Object),
    Set {
        object: Object,
        key: String,
        value: String,
    },
    Remove {
        object: Object,
        key: String,
    },
    SeedText {
        object: Object,
        key: String,
        value: String,
    },
    PrefixText {
        object: Object,
        key: String,
        value: String,
    },
    SuffixText {
        object: Object,
        key: String,
        value: String,
    },
}

impl Action {
    pub fn object(&self, id: EventId) -> Object {
        match self {
            Self::CreateNode(graph) => Object::Node(NodeId {
                graph: *graph,
                birth: id,
            }),
            Self::CreateEdge { graph, .. } => Object::Edge(EdgeId {
                graph: *graph,
                birth: id,
            }),
            Self::Delete(object)
            | Self::Restore(object)
            | Self::Set { object, .. }
            | Self::Remove { object, .. }
            | Self::SeedText { object, .. }
            | Self::PrefixText { object, .. }
            | Self::SuffixText { object, .. } => *object,
        }
    }

    fn is_lifecycle(&self) -> bool {
        matches!(
            self,
            Self::CreateNode(_) | Self::CreateEdge { .. } | Self::Delete(_) | Self::Restore(_)
        )
    }
}

#[derive(Clone, Debug)]
pub struct Event {
    pub id: EventId,
    /// The complete transitive ancestor set, captured BEFORE executing action.
    pub ancestors: Version,
    pub action: Action,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Properties {
    pub scalars: BTreeMap<String, String>,
    pub texts: BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Node {
    pub alive: bool,
    pub properties: Properties,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edge {
    pub source: NodeId,
    pub target: NodeId,
    pub alive: bool,
    pub visible: bool,
    pub properties: Properties,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub nodes: BTreeMap<NodeId, Node>,
    pub edges: BTreeMap<EdgeId, Edge>,
}

impl Snapshot {
    pub fn visible_nodes(&self) -> BTreeSet<NodeId> {
        self.nodes
            .iter()
            .filter_map(|(id, node)| node.alive.then_some(*id))
            .collect()
    }

    pub fn visible_edges(&self) -> BTreeSet<EdgeId> {
        self.edges
            .iter()
            .filter_map(|(id, edge)| edge.visible.then_some(*id))
            .collect()
    }

    // These queries deliberately scan immutable records, never maintain an
    // incremental adjacency cache, and preserve distinct parallel edge IDs.
    pub fn incoming(&self, node: NodeId) -> BTreeSet<EdgeId> {
        self.edges
            .iter()
            .filter_map(|(id, e)| (e.visible && e.target == node).then_some(*id))
            .collect()
    }

    pub fn outgoing(&self, node: NodeId) -> BTreeSet<EdgeId> {
        self.edges
            .iter()
            .filter_map(|(id, e)| (e.visible && e.source == node).then_some(*id))
            .collect()
    }

    pub fn successors(&self, node: NodeId) -> BTreeSet<NodeId> {
        self.edges
            .values()
            .filter_map(|e| (e.visible && e.source == node).then_some(e.target))
            .collect()
    }

    pub fn predecessors(&self, node: NodeId) -> BTreeSet<NodeId> {
        self.edges
            .values()
            .filter_map(|e| (e.visible && e.target == node).then_some(e.source))
            .collect()
    }

    pub fn properties(&self, object: Object) -> &Properties {
        match object {
            Object::Node(id) => &self.nodes[&id].properties,
            Object::Edge(id) => &self.edges[&id].properties,
        }
    }
}

#[derive(Debug, Default)]
pub struct Oracle {
    pub events: Vec<Event>,
}

impl Oracle {
    pub fn next_id(&self) -> EventId {
        EventId(self.events.len())
    }

    pub fn is_closed(&self, version: &Version) -> bool {
        version.iter().all(|id| {
            self.events
                .get(id.0)
                .is_some_and(|event| event.ancestors.is_subset(version))
        })
    }

    /// Delivered bytes may be pending. Compute the largest causally closed
    /// subset of received actions independently of the production DocState.
    pub fn closed_subset(&self, received: &Version) -> Version {
        received
            .iter()
            .filter(|id| {
                self.events
                    .get(id.0)
                    .is_some_and(|event| event.ancestors.is_subset(received))
            })
            .copied()
            .collect()
    }

    pub fn validate(&self, at: &Version, action: &Action) -> Result<(), &'static str> {
        if !self.is_closed(at) {
            return Err("version is not causally closed");
        }
        let exists = |object: Object| {
            at.contains(&object.birth())
                && self.events[object.birth().0].action.object(object.birth()) == object
                && matches!(
                    self.events[object.birth().0].action,
                    Action::CreateNode(_) | Action::CreateEdge { .. }
                )
        };
        match action {
            Action::CreateNode(_) => Ok(()),
            Action::CreateEdge {
                graph,
                source,
                target,
            } => {
                if source.graph != *graph || target.graph != *graph {
                    return Err("cross-graph endpoint");
                }
                if !exists(Object::Node(*source)) || !exists(Object::Node(*target)) {
                    return Err("missing endpoint");
                }
                Ok(())
            }
            _ if exists(action.object(self.next_id())) => Ok(()),
            _ => Err("missing object"),
        }
    }

    pub fn append(&mut self, at: &Version, action: Action) -> EventId {
        self.validate(at, &action).unwrap();
        let id = self.next_id();
        self.events.push(Event {
            id,
            ancestors: at.clone(),
            action,
        });
        id
    }

    pub fn append_to(&mut self, at: &mut Version, action: Action) -> EventId {
        let id = self.append(at, action);
        at.insert(id);
        id
    }

    /// Evaluate exactly this boundary, ignoring all later and unrelated ops.
    pub fn snapshot(&self, graph: Graph, at: &Version) -> Snapshot {
        assert!(self.is_closed(at), "oracle requires a causal boundary");
        let events: Vec<_> = at
            .iter()
            .map(|id| &self.events[id.0])
            .filter(|e| e.action.object(e.id).graph() == graph)
            .collect();
        let mut result = Snapshot::default();
        for event in &events {
            if let Action::CreateNode(_) = event.action {
                let Object::Node(id) = event.action.object(event.id) else {
                    unreachable!()
                };
                result.nodes.insert(
                    id,
                    Node {
                        alive: alive(Object::Node(id), &events),
                        properties: properties(Object::Node(id), &events),
                    },
                );
            }
        }
        for event in &events {
            if let Action::CreateEdge { source, target, .. } = event.action {
                let Object::Edge(id) = event.action.object(event.id) else {
                    unreachable!()
                };
                let alive = alive(Object::Edge(id), &events);
                result.edges.insert(
                    id,
                    Edge {
                        source,
                        target,
                        alive,
                        visible: alive
                            && result.nodes[&source].alive
                            && result.nodes[&target].alive,
                        properties: properties(Object::Edge(id), &events),
                    },
                );
            }
        }
        result
    }
}

/// Pairwise happens-before comparison, NOT Lamport order or a delete-tag set.
fn maxima<'a>(events: &[&'a Event]) -> Vec<&'a Event> {
    events
        .iter()
        .filter(|candidate| {
            !events
                .iter()
                .any(|other| other.ancestors.contains(&candidate.id))
        })
        .copied()
        .collect()
}

fn alive(object: Object, events: &[&Event]) -> bool {
    let lifecycle: Vec<_> = events
        .iter()
        .filter(|e| e.action.is_lifecycle() && e.action.object(e.id) == object)
        .copied()
        .collect();
    assert!(!lifecycle.is_empty());
    maxima(&lifecycle)
        .iter()
        .all(|e| !matches!(e.action, Action::Delete(_)))
}

fn properties(object: Object, events: &[&Event]) -> Properties {
    let mut scalars: BTreeMap<&str, Vec<&Event>> = BTreeMap::new();
    let mut texts: BTreeMap<&str, Vec<&Event>> = BTreeMap::new();
    for event in events {
        if event.action.object(event.id) != object {
            continue;
        }
        match &event.action {
            Action::Set { key, .. } | Action::Remove { key, .. } => {
                scalars.entry(key).or_default().push(event);
            }
            Action::SeedText { key, .. }
            | Action::PrefixText { key, .. }
            | Action::SuffixText { key, .. } => {
                texts.entry(key).or_default().push(event);
            }
            _ => {}
        }
    }
    let mut result = Properties::default();
    for (key, writes) in scalars {
        let winners = maxima(&writes);
        assert_eq!(
            winners.len(),
            1,
            "scalar test keys must have one causal writer"
        );
        if let Action::Set { value, .. } = &winners[0].action {
            result.scalars.insert(key.to_string(), value.clone());
        }
    }
    for (key, edits) in texts {
        let mut seed = None;
        let mut prefix = None;
        let mut suffix = None;
        for event in &edits {
            let (slot, value) = match &event.action {
                Action::SeedText { value, .. } => (&mut seed, value),
                Action::PrefixText { value, .. } => (&mut prefix, value),
                Action::SuffixText { value, .. } => (&mut suffix, value),
                _ => unreachable!(),
            };
            assert!(slot.replace((event.id, value.as_str())).is_none());
        }
        let (seed_id, seed_text) = seed.expect("text edits require a shared seed");
        assert!(!seed_text.is_empty());
        for event in &edits {
            assert!(event.id == seed_id || event.ancestors.contains(&seed_id));
        }
        result.texts.insert(
            key.to_string(),
            format!(
                "{}{}{}",
                prefix.map_or("", |(_, value)| value),
                seed_text,
                suffix.map_or("", |(_, value)| value)
            ),
        );
    }
    result
}
