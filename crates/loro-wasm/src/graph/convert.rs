//! JS graph diffs use decimal ID strings, including lifecycle operation tags.
use std::collections::BTreeMap;

use loro_common::{GraphEdgeId, GraphNodeId, ID};
use loro_internal::container::graph::{GraphChange, GraphDiff, GraphEdge, GraphNode, GraphOp};
use serde::{Deserialize, Serialize};
use wasm_bindgen::JsValue;

use super::{edge_id, node_id, to_js};
use crate::JsResult;

fn operation_id(value: &str) -> JsResult<ID> {
    Ok(node_id(value)?.id())
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct NodeRecord {
    id: String,
    alive: bool,
    visible: bool,
    delete_tags: Vec<String>,
}

impl From<&GraphNode> for NodeRecord {
    fn from(node: &GraphNode) -> Self {
        Self {
            id: node.id.to_string(),
            alive: node.alive,
            visible: node.visible,
            delete_tags: node.delete_tags.iter().map(ToString::to_string).collect(),
        }
    }
}

impl TryFrom<NodeRecord> for GraphNode {
    type Error = JsValue;

    fn try_from(node: NodeRecord) -> JsResult<Self> {
        Ok(Self {
            id: node_id(&node.id)?,
            alive: node.alive,
            visible: node.visible,
            delete_tags: node
                .delete_tags
                .iter()
                .map(|id| operation_id(id))
                .collect::<JsResult<_>>()?,
        })
    }
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct EdgeRecord {
    id: String,
    source: String,
    target: String,
    alive: bool,
    visible: bool,
    delete_tags: Vec<String>,
}

impl From<&GraphEdge> for EdgeRecord {
    fn from(edge: &GraphEdge) -> Self {
        Self {
            id: edge.id.to_string(),
            source: edge.source.to_string(),
            target: edge.target.to_string(),
            alive: edge.alive,
            visible: edge.visible,
            delete_tags: edge.delete_tags.iter().map(ToString::to_string).collect(),
        }
    }
}

impl TryFrom<EdgeRecord> for GraphEdge {
    type Error = JsValue;

    fn try_from(edge: EdgeRecord) -> JsResult<Self> {
        Ok(Self {
            id: edge_id(&edge.id)?,
            source: node_id(&edge.source)?,
            target: node_id(&edge.target)?,
            alive: edge.alive,
            visible: edge.visible,
            delete_tags: edge
                .delete_tags
                .iter()
                .map(|id| operation_id(id))
                .collect::<JsResult<_>>()?,
        })
    }
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Operation {
    CreateNode {
        id: String,
    },
    CreateEdge {
        id: String,
        source: String,
        target: String,
    },
    DeleteNode {
        id: String,
    },
    DeleteEdge {
        id: String,
    },
    RestoreNode {
        id: String,
        deletes: Vec<String>,
    },
    RestoreEdge {
        id: String,
        deletes: Vec<String>,
    },
}

impl From<&GraphOp> for Operation {
    fn from(op: &GraphOp) -> Self {
        match op {
            GraphOp::CreateNode { id } => Self::CreateNode { id: id.to_string() },
            GraphOp::CreateEdge { id, source, target } => Self::CreateEdge {
                id: id.to_string(),
                source: source.to_string(),
                target: target.to_string(),
            },
            GraphOp::DeleteNode { id } => Self::DeleteNode { id: id.to_string() },
            GraphOp::DeleteEdge { id } => Self::DeleteEdge { id: id.to_string() },
            GraphOp::RestoreNode { id, deletes } => Self::RestoreNode {
                id: id.to_string(),
                deletes: deletes.iter().map(ToString::to_string).collect(),
            },
            GraphOp::RestoreEdge { id, deletes } => Self::RestoreEdge {
                id: id.to_string(),
                deletes: deletes.iter().map(ToString::to_string).collect(),
            },
        }
    }
}

impl TryFrom<Operation> for GraphOp {
    type Error = JsValue;

    fn try_from(op: Operation) -> JsResult<Self> {
        Ok(match op {
            Operation::CreateNode { id } => Self::CreateNode { id: node_id(&id)? },
            Operation::CreateEdge { id, source, target } => Self::CreateEdge {
                id: edge_id(&id)?,
                source: node_id(&source)?,
                target: node_id(&target)?,
            },
            Operation::DeleteNode { id } => Self::DeleteNode { id: node_id(&id)? },
            Operation::DeleteEdge { id } => Self::DeleteEdge { id: edge_id(&id)? },
            Operation::RestoreNode { id, deletes } => Self::RestoreNode {
                id: node_id(&id)?,
                deletes: deletes
                    .iter()
                    .map(|id| operation_id(id))
                    .collect::<JsResult<_>>()?,
            },
            Operation::RestoreEdge { id, deletes } => Self::RestoreEdge {
                id: edge_id(&id)?,
                deletes: deletes
                    .iter()
                    .map(|id| operation_id(id))
                    .collect::<JsResult<_>>()?,
            },
        })
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Change {
    id: String,
    op: Operation,
    forward: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Diff {
    ops: Vec<Change>,
    nodes: BTreeMap<String, Option<NodeRecord>>,
    edges: BTreeMap<String, Option<EdgeRecord>>,
}

pub(crate) fn diff_to_js(diff: &GraphDiff) -> JsResult<JsValue> {
    to_js(&Diff {
        ops: diff
            .ops
            .iter()
            .map(|change| Change {
                id: change.id.to_string(),
                op: (&change.op).into(),
                forward: change.forward,
            })
            .collect(),
        nodes: diff
            .nodes
            .iter()
            .map(|(id, node)| (id.to_string(), node.as_ref().map(Into::into)))
            .collect(),
        edges: diff
            .edges
            .iter()
            .map(|(id, edge)| (id.to_string(), edge.as_ref().map(Into::into)))
            .collect(),
    })
}

pub(crate) fn diff_from_js(value: JsValue) -> JsResult<GraphDiff> {
    let diff: Diff = serde_wasm_bindgen::from_value(value)
        .map_err(|error| JsValue::from_str(&format!("Invalid Graph diff: {error}")))?;
    let ops = diff
        .ops
        .into_iter()
        .map(|change| {
            let id = operation_id(&change.id)?;
            let op: GraphOp = change.op.try_into()?;
            op.validate(id)?;
            Ok(GraphChange {
                id,
                op,
                forward: change.forward,
            })
        })
        .collect::<JsResult<_>>()?;
    let nodes = diff
        .nodes
        .into_iter()
        .map(|(key, value)| {
            let key = node_id(&key)?;
            let record: Option<GraphNode> = value.map(TryInto::try_into).transpose()?;
            if record.as_ref().is_some_and(|node| node.id != key) {
                return Err(JsValue::from_str(
                    "Graph node record key does not match its ID",
                ));
            }
            Ok((key, record))
        })
        .collect::<JsResult<BTreeMap<GraphNodeId, _>>>()?;
    let edges = diff
        .edges
        .into_iter()
        .map(|(key, value)| {
            let key = edge_id(&key)?;
            let record: Option<GraphEdge> = value.map(TryInto::try_into).transpose()?;
            if record.as_ref().is_some_and(|edge| edge.id != key) {
                return Err(JsValue::from_str(
                    "Graph edge record key does not match its ID",
                ));
            }
            Ok((key, record))
        })
        .collect::<JsResult<BTreeMap<GraphEdgeId, _>>>()?;
    Ok(GraphDiff { ops, nodes, edges })
}
