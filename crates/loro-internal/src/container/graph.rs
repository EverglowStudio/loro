//! Native graph operations. Graph relations never become container ownership links.
use loro_common::{ContainerID, GraphEdgeId, GraphNodeId, LoroError, LoroResult, ID};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphOp {
    CreateNode {
        id: GraphNodeId,
    },
    CreateEdge {
        id: GraphEdgeId,
        source: GraphNodeId,
        target: GraphNodeId,
    },
    DeleteNode {
        id: GraphNodeId,
    },
    DeleteEdge {
        id: GraphEdgeId,
    },
    RestoreNode {
        id: GraphNodeId,
        #[serde(with = "id_vec")]
        deletes: Vec<ID>,
    },
    RestoreEdge {
        id: GraphEdgeId,
        #[serde(with = "id_vec")]
        deletes: Vec<ID>,
    },
}
impl GraphOp {
    pub fn created_meta(&self) -> Option<ContainerID> {
        match self {
            Self::CreateNode { id } => Some(id.associated_meta_container()),
            Self::CreateEdge { id, .. } => Some(id.associated_meta_container()),
            _ => None,
        }
    }
    pub fn validate(&self, op_id: ID) -> LoroResult<()> {
        let bad = || LoroError::DecodeError("Invalid graph operation identity or reference".into());
        let check = |id: ID| if id.counter < 0 { Err(bad()) } else { Ok(()) };
        check(op_id)?;
        match self {
            Self::CreateNode { id } => {
                if id.id() != op_id {
                    return Err(bad());
                }
            }
            Self::CreateEdge { id, source, target } => {
                if id.id() != op_id {
                    return Err(bad());
                }
                check(source.id())?;
                check(target.id())?;
            }
            Self::DeleteNode { id } => check(id.id())?,
            Self::DeleteEdge { id } => check(id.id())?,
            Self::RestoreNode { id, deletes } => {
                check(id.id())?;
                for tag in deletes {
                    check(*tag)?;
                }
                if deletes
                    .iter()
                    .collect::<std::collections::BTreeSet<_>>()
                    .len()
                    != deletes.len()
                {
                    return Err(bad());
                }
            }
            Self::RestoreEdge { id, deletes } => {
                check(id.id())?;
                for tag in deletes {
                    check(*tag)?;
                }
                if deletes
                    .iter()
                    .collect::<std::collections::BTreeSet<_>>()
                    .len()
                    != deletes.len()
                {
                    return Err(bad());
                }
            }
        }
        Ok(())
    }
    pub fn encoded(&self) -> Vec<u8> {
        postcard::to_stdvec(self).expect("Graph operation serialization")
    }
    pub fn decode(bytes: &[u8], id: ID) -> LoroResult<Self> {
        let (op, rest): (Self, _) = postcard::take_from_bytes(bytes)
            .map_err(|_| LoroError::DecodeError("Invalid graph operation".into()))?;
        if !rest.is_empty() {
            return Err(LoroError::DecodeError(
                "Trailing graph operation bytes".into(),
            ));
        }
        op.validate(id)?;
        Ok(op)
    }
}

/// Operation identity is preserved through inverse diffs for selective undo.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphChange {
    #[serde(with = "op_id")]
    pub id: ID,
    pub op: GraphOp,
    pub forward: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphNode {
    pub id: GraphNodeId,
    pub alive: bool,
    pub visible: bool,
    #[serde(with = "id_vec")]
    pub delete_tags: Vec<ID>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphEdge {
    pub id: GraphEdgeId,
    pub source: GraphNodeId,
    pub target: GraphNodeId,
    pub alive: bool,
    pub visible: bool,
    #[serde(with = "id_vec")]
    pub delete_tags: Vec<ID>,
}

/// Changed records include incident edges whose visibility changed with an endpoint.
/// `None` means that checkout removed the object's creation from the selected history.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphDiff {
    pub ops: Vec<GraphChange>,
    pub nodes: std::collections::BTreeMap<GraphNodeId, Option<GraphNode>>,
    pub edges: std::collections::BTreeMap<GraphEdgeId, Option<GraphEdge>>,
}
impl GraphDiff {
    pub fn is_empty(&self) -> bool {
        self.ops.is_empty() && self.nodes.is_empty() && self.edges.is_empty()
    }
    pub fn compose(mut self, other: Self) -> Self {
        self.ops.extend(other.ops);
        self.nodes.extend(other.nodes);
        self.edges.extend(other.edges);
        self
    }
}

impl GraphOp {
    pub(crate) fn references(&self) -> Vec<(ID, Option<ID>, bool)> {
        // (referenced creation/delete, expected deleted object's creation, is edge)
        match self {
            Self::CreateNode { .. } => vec![],
            Self::CreateEdge { source, target, .. } => {
                vec![(source.id(), None, false), (target.id(), None, false)]
            }
            Self::DeleteNode { id } => vec![(id.id(), None, false)],
            Self::DeleteEdge { id } => vec![(id.id(), None, true)],
            Self::RestoreNode { id, deletes } => std::iter::once((id.id(), None, false))
                .chain(deletes.iter().map(|d| (*d, Some(id.id()), false)))
                .collect(),
            Self::RestoreEdge { id, deletes } => std::iter::once((id.id(), None, true))
                .chain(deletes.iter().map(|d| (*d, Some(id.id()), true)))
                .collect(),
        }
    }
}

impl crate::OpLog {
    /// Validate graph references against native operation identities and the operation DAG.
    /// Traversal is bounded by the referenced ancestors; no per-operation document VV is stored.
    pub(crate) fn validate_graph_import(
        &self,
        imported: &crate::version::VersionRange,
    ) -> LoroResult<()> {
        use crate::{
            dag::{Dag, DagNode},
            span::{HasId, HasLamport},
        };
        use loro_common::IdSpan;
        use std::collections::BTreeSet;
        let invalid = || {
            LoroError::DecodeError(
                "Graph reference must name an observed operation in the same graph".into(),
            )
        };
        for (peer, (start, end)) in imported.iter() {
            for op in self.iter_ops(IdSpan::new(*peer, *start, *end)) {
                let raw = op.op();
                let Some(graph) = raw.content.as_graph() else {
                    continue;
                };
                let id = op.id();
                graph.validate(id)?;
                let refs = graph.references();
                for (reference, deleted, is_edge) in &refs {
                    if let Some(target) = self.get_op_that_includes(*reference) {
                        if target.container != op.op().container
                            || target.counter != reference.counter
                        {
                            return Err(invalid());
                        }
                        let valid = match (
                            target.content.as_graph().map(|g| g.as_ref()),
                            deleted,
                            is_edge,
                        ) {
                            (Some(GraphOp::CreateNode { id }), None, false) => {
                                id.id() == *reference
                            }
                            (Some(GraphOp::CreateEdge { id, .. }), None, true) => {
                                id.id() == *reference
                            }
                            (Some(GraphOp::DeleteNode { id }), Some(target), false) => {
                                id.id() == *target
                            }
                            (Some(GraphOp::DeleteEdge { id }), Some(target), true) => {
                                id.id() == *target
                            }
                            _ => false,
                        };
                        if !valid {
                            return Err(invalid());
                        }
                    } else if !self.shallow_since_vv().includes_id(*reference) {
                        return Err(invalid());
                    }
                }
                let mut remaining: BTreeSet<ID> = refs.into_iter().map(|r| r.0).collect();
                // Every valid operation after a shallow root observes that complete root.
                remaining.retain(|id| !self.shallow_since_vv().includes_id(*id));
                let mut stack: Vec<_> = self.dag.find_deps_of_id(id).iter().collect();
                let mut visited = BTreeSet::new();
                while !remaining.is_empty() {
                    let Some(tip) = stack.pop() else {
                        return Err(invalid());
                    };
                    if !visited.insert(tip) {
                        continue;
                    }
                    let Some(node) = self.dag.get(tip) else {
                        continue;
                    };
                    remaining.retain(|r| {
                        !(r.peer == tip.peer
                            && node.id_start().counter <= r.counter
                            && r.counter <= tip.counter)
                    });
                    if remaining.is_empty() {
                        break;
                    }
                    if remaining.iter().all(|r| {
                        self.dag
                            .get_lamport(r)
                            .is_some_and(|lp| lp >= node.lamport())
                    }) {
                        continue;
                    }
                    stack.extend(node.deps().iter());
                }
            }
        }
        Ok(())
    }
}

mod op_id {
    use super::*;
    pub fn serialize<S: serde::Serializer>(id: &ID, s: S) -> Result<S::Ok, S::Error> {
        if s.is_human_readable() {
            s.serialize_str(&id.to_string())
        } else {
            id.serialize(s)
        }
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(d: D) -> Result<ID, D::Error> {
        if d.is_human_readable() {
            let s = String::deserialize(d)?;
            ID::try_from(s.as_str()).map_err(serde::de::Error::custom)
        } else {
            ID::deserialize(d)
        }
    }
}
mod id_vec {
    use super::*;
    pub fn serialize<S: serde::Serializer>(ids: &[ID], s: S) -> Result<S::Ok, S::Error> {
        if s.is_human_readable() {
            ids.iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .serialize(s)
        } else {
            ids.serialize(s)
        }
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<ID>, D::Error> {
        if d.is_human_readable() {
            Vec::<String>::deserialize(d)?
                .into_iter()
                .map(|s| ID::try_from(s.as_str()).map_err(serde::de::Error::custom))
                .collect()
        } else {
            Vec::<ID>::deserialize(d)
        }
    }
}
