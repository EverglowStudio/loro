//! JS graph diffs use decimal ID strings, including lifecycle operation tags.
use std::collections::BTreeMap;

use loro_common::{GraphEdgeId, GraphNodeId, IdFull, ID};
use loro_internal::container::graph::{
    GraphChange, GraphDiff, GraphEdge, GraphNode, GraphOp, GraphOrderDelta, GraphOrderValue,
    GraphPosition, OrderedGraphEdge,
};
use serde::{Deserialize, Serialize};
use wasm_bindgen::JsValue;

use super::{edge_id, node_id, order_error, to_js};
use crate::JsResult;

fn operation_id(value: &str) -> JsResult<ID> {
    Ok(node_id(value)?.id())
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OrderWriter {
    id: String,
    lamport: u32,
}

impl From<IdFull> for OrderWriter {
    fn from(id: IdFull) -> Self {
        Self {
            id: id.id().to_string(),
            lamport: id.lamport,
        }
    }
}

impl TryFrom<OrderWriter> for IdFull {
    type Error = JsValue;

    fn try_from(writer: OrderWriter) -> JsResult<Self> {
        let id = operation_id(&writer.id)?;
        Ok(IdFull::new(id.peer, id.counter, writer.lamport))
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct OrderedEdge {
    edge_id: String,
    target: String,
    position: String,
}

impl From<&OrderedGraphEdge> for OrderedEdge {
    fn from(edge: &OrderedGraphEdge) -> Self {
        Self {
            edge_id: edge.edge_id.to_string(),
            target: edge.target.to_string(),
            position: edge.position.to_string(),
        }
    }
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
    position: String,
    last_order: OrderWriter,
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
            position: edge.position.to_string(),
            last_order: edge.last_order.into(),
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
            position: GraphPosition::try_from_hex(&edge.position).map_err(order_error)?,
            last_order: edge.last_order.try_into()?,
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
        position: String,
    },
    SetEdgeOrder {
        id: String,
        position: String,
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
            GraphOp::CreateEdge {
                id,
                source,
                target,
                position,
            } => Self::CreateEdge {
                id: id.to_string(),
                source: source.to_string(),
                target: target.to_string(),
                position: position.to_string(),
            },
            GraphOp::SetEdgeOrder { id, position } => Self::SetEdgeOrder {
                id: id.to_string(),
                position: position.to_string(),
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
            Operation::CreateEdge {
                id,
                source,
                target,
                position,
            } => Self::CreateEdge {
                id: edge_id(&id)?,
                source: node_id(&source)?,
                target: node_id(&target)?,
                position: GraphPosition::try_from_hex(&position).map_err(order_error)?,
            },
            Operation::SetEdgeOrder { id, position } => Self::SetEdgeOrder {
                id: edge_id(&id)?,
                position: GraphPosition::try_from_hex(&position).map_err(order_error)?,
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
    lamport: u32,
    op: Operation,
    forward: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OrderValue {
    position: String,
    last_order: OrderWriter,
}

impl From<&GraphOrderValue> for OrderValue {
    fn from(value: &GraphOrderValue) -> Self {
        Self {
            position: value.position.to_string(),
            last_order: value.last_order.into(),
        }
    }
}

impl TryFrom<OrderValue> for GraphOrderValue {
    type Error = JsValue;

    fn try_from(value: OrderValue) -> JsResult<Self> {
        Ok(Self {
            position: GraphPosition::try_from_hex(&value.position).map_err(order_error)?,
            last_order: value.last_order.try_into()?,
        })
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OrderDelta {
    source: String,
    before: Option<OrderValue>,
    after: Option<OrderValue>,
}

impl From<&GraphOrderDelta> for OrderDelta {
    fn from(delta: &GraphOrderDelta) -> Self {
        Self {
            source: delta.source.to_string(),
            before: delta.before.as_ref().map(Into::into),
            after: delta.after.as_ref().map(Into::into),
        }
    }
}

impl TryFrom<OrderDelta> for GraphOrderDelta {
    type Error = JsValue;

    fn try_from(delta: OrderDelta) -> JsResult<Self> {
        Ok(Self {
            source: node_id(&delta.source)?,
            before: delta.before.map(TryInto::try_into).transpose()?,
            after: delta.after.map(TryInto::try_into).transpose()?,
        })
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Diff {
    ops: Vec<Change>,
    nodes: BTreeMap<String, Option<NodeRecord>>,
    edges: BTreeMap<String, Option<EdgeRecord>>,
    orders: BTreeMap<String, OrderDelta>,
}

pub(crate) fn diff_to_js(diff: &GraphDiff) -> JsResult<JsValue> {
    to_js(&Diff {
        ops: diff
            .ops
            .iter()
            .map(|change| Change {
                id: change.id.to_string(),
                lamport: change.lamport,
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
        orders: diff
            .orders
            .iter()
            .map(|(id, delta)| (id.to_string(), delta.into()))
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
                lamport: change.lamport,
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
    let orders = diff
        .orders
        .into_iter()
        .map(|(id, delta)| Ok((edge_id(&id)?, delta.try_into()?)))
        .collect::<JsResult<BTreeMap<GraphEdgeId, GraphOrderDelta>>>()?;
    Ok(GraphDiff {
        ops,
        nodes,
        edges,
        orders,
    })
}
