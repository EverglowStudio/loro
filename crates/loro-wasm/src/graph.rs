//! Native graph bindings. Topology and lifecycle semantics remain in loro-internal.
use std::sync::Arc;

use js_sys::{Array, Object, Reflect};
use loro_common::{GraphEdgeId, GraphNodeId};
use loro_internal::{
    graph::{self, GraphRepairError, GraphScope, RepairPolicy, RepairReason},
    handler::{GraphHandler, Handler},
    HandlerTrait,
};
use serde::Serialize;
use wasm_bindgen::prelude::*;

use crate::{
    convert::handler_to_js_value, frontiers_to_ids, observer, put_event_in_pending_queue,
    subscription_to_js_function_callback, JsContainerID, JsContainerOrUndefined, JsIDs, JsResult,
    LoroMap,
};

mod convert;
mod types;
pub(crate) use convert::{diff_from_js, diff_to_js};
use convert::{EdgeRecord, NodeRecord};
use types::*;

fn to_js<T: Serialize>(value: &T) -> JsResult<JsValue> {
    value
        .serialize(
            &serde_wasm_bindgen::Serializer::new()
                .serialize_maps_as_objects(true)
                .serialize_missing_as_null(true),
        )
        .map_err(|error| JsValue::from_str(&format!("Cannot serialize Graph value: {error}")))
}

fn valid_id(value: &str) -> bool {
    value.split_once('@').is_some_and(|(counter, peer)| {
        !counter.is_empty()
            && !peer.is_empty()
            && counter.bytes().all(|ch| ch.is_ascii_digit())
            && peer.bytes().all(|ch| ch.is_ascii_digit())
    })
}

fn node_id(value: &str) -> JsResult<GraphNodeId> {
    if !valid_id(value) {
        return Err(JsValue::from_str(
            "Graph node ID must be a counter@peer decimal string",
        ));
    }
    GraphNodeId::try_from(value).map_err(Into::into)
}

fn edge_id(value: &str) -> JsResult<GraphEdgeId> {
    if !valid_id(value) {
        return Err(JsValue::from_str(
            "Graph edge ID must be a counter@peer decimal string",
        ));
    }
    GraphEdgeId::try_from(value).map_err(Into::into)
}

fn node_arg(value: &JsGraphNodeId) -> JsResult<GraphNodeId> {
    node_id(&value.as_string().ok_or("Graph node ID must be a string")?)
}

fn edge_arg(value: &JsGraphEdgeId) -> JsResult<GraphEdgeId> {
    edge_id(&value.as_string().ok_or("Graph edge ID must be a string")?)
}

fn id_array<T: ToString>(ids: impl IntoIterator<Item = T>) -> JsValue {
    Array::from_iter(ids.into_iter().map(|id| JsValue::from_str(&id.to_string()))).into()
}

fn scope(selected: Option<JsGraphEdgeIds>) -> JsResult<GraphScope> {
    let Some(selected) = selected else {
        return Ok(GraphScope::AllVisible);
    };
    if !Array::is_array(&selected) {
        return Err(JsValue::from_str(
            "Selected graph edges must be an array of ID strings",
        ));
    }
    let ids = Array::from(&selected)
        .iter()
        .map(|id| edge_id(&id.as_string().ok_or("Graph edge ID must be a string")?))
        .collect::<JsResult<_>>()?;
    Ok(GraphScope::Selected(ids))
}

fn scope_value(scope: &GraphScope) -> JsResult<JsValue> {
    let value = Object::new();
    match scope {
        GraphScope::AllVisible => {
            Reflect::set(&value, &"type".into(), &"allVisible".into())?;
        }
        GraphScope::Selected(ids) => {
            Reflect::set(&value, &"type".into(), &"selected".into())?;
            Reflect::set(&value, &"edges".into(), &id_array(ids))?;
        }
    }
    Ok(value.into())
}

fn repair_error(error: GraphRepairError) -> JsValue {
    let code = match &error {
        GraphRepairError::Loro(_) => "LoroError",
        GraphRepairError::UncommittedChanges => "UncommittedChanges",
        GraphRepairError::StalePlan => "StalePlan",
        GraphRepairError::WrongGraph => "WrongGraph",
        GraphRepairError::DetachedContainer => "DetachedContainer",
        GraphRepairError::InvalidSelection(_) => "InvalidSelection",
        GraphRepairError::InvalidPlan => "InvalidPlan",
    };
    let value: JsValue = JsError::new(&error.to_string()).into();
    let _ = Reflect::set(&value, &"code".into(), &code.into());
    value
}

/// A directed property multigraph. Cycles, parallel edges and self-loops are valid.
#[derive(Clone)]
#[wasm_bindgen]
pub struct LoroGraph {
    pub(crate) handler: GraphHandler,
}

impl Default for LoroGraph {
    fn default() -> Self {
        Self::new()
    }
}

#[wasm_bindgen]
impl LoroGraph {
    /// Create an editable detached graph. Insert it into a container to attach it.
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        Self {
            handler: GraphHandler::new_detached(),
        }
    }

    pub fn kind(&self) -> JsGraphStr {
        JsValue::from_str("Graph").into()
    }

    #[wasm_bindgen(getter)]
    pub fn id(&self) -> JsContainerID {
        JsValue::from(&self.handler.id()).into()
    }

    #[wasm_bindgen(js_name = "createNode")]
    pub fn create_node(&self) -> JsResult<JsGraphNodeId> {
        Ok(JsValue::from_str(&self.handler.create_node()?.to_string()).into())
    }

    /// Allocate a distinct edge. Endpoints are immutable; parallel edges and cycles are allowed.
    #[wasm_bindgen(js_name = "createEdge")]
    pub fn create_edge(
        &self,
        source: &JsGraphNodeId,
        target: &JsGraphNodeId,
    ) -> JsResult<JsGraphEdgeId> {
        Ok(JsValue::from_str(
            &self
                .handler
                .create_edge(node_arg(source)?, node_arg(target)?)?
                .to_string(),
        )
        .into())
    }

    /// Hide the node and its incident edges without deleting those edge records.
    #[wasm_bindgen(js_name = "deleteNode")]
    pub fn delete_node(&self, id: &JsGraphNodeId) -> JsResult<()> {
        self.handler.delete_node(node_arg(id)?)?;
        Ok(())
    }

    #[wasm_bindgen(js_name = "deleteEdge")]
    pub fn delete_edge(&self, id: &JsGraphEdgeId) -> JsResult<()> {
        self.handler.delete_edge(edge_arg(id)?)?;
        Ok(())
    }

    /// Restore only observed deletions. Other concurrent deletion tags remain effective.
    #[wasm_bindgen(js_name = "restoreNode")]
    pub fn restore_node(&self, id: &JsGraphNodeId) -> JsResult<()> {
        self.handler.restore_node(node_arg(id)?)?;
        Ok(())
    }

    #[wasm_bindgen(js_name = "restoreEdge")]
    pub fn restore_edge(&self, id: &JsGraphEdgeId) -> JsResult<()> {
        self.handler.restore_edge(edge_arg(id)?)?;
        Ok(())
    }

    /// Visible node IDs in native ID order.
    pub fn nodes(&self) -> JsGraphNodeIds {
        id_array(self.handler.nodes()).into()
    }

    /// Visible edge IDs in native ID order.
    pub fn edges(&self) -> JsGraphEdgeIds {
        id_array(self.handler.edges()).into()
    }

    #[wasm_bindgen(js_name = "nodeCount")]
    pub fn node_count(&self) -> usize {
        self.handler.node_count()
    }

    #[wasm_bindgen(js_name = "edgeCount")]
    pub fn edge_count(&self) -> usize {
        self.handler.edge_count()
    }

    /// Return a node only when it is visible.
    #[wasm_bindgen(js_name = "getNode")]
    pub fn get_node(&self, id: &JsGraphNodeId) -> JsResult<JsGraphNodeRecordOrUndefined> {
        Ok(match self.handler.get_node(node_arg(id)?) {
            Some(node) => to_js(&NodeRecord::from(&node))?,
            None => JsValue::UNDEFINED,
        }
        .into())
    }

    /// Return an edge only when it and both endpoints are alive.
    #[wasm_bindgen(js_name = "getEdge")]
    pub fn get_edge(&self, id: &JsGraphEdgeId) -> JsResult<JsGraphEdgeRecordOrUndefined> {
        Ok(match self.handler.get_edge(edge_arg(id)?) {
            Some(edge) => to_js(&EdgeRecord::from(&edge))?,
            None => JsValue::UNDEFINED,
        }
        .into())
    }

    /// Diagnostic lookup, including deleted records.
    #[wasm_bindgen(js_name = "nodeRecord")]
    pub fn node_record(&self, id: &JsGraphNodeId) -> JsResult<JsGraphNodeRecordOrUndefined> {
        Ok(match self.handler.node_record(node_arg(id)?) {
            Some(node) => to_js(&NodeRecord::from(&node))?,
            None => JsValue::UNDEFINED,
        }
        .into())
    }

    /// Diagnostic lookup, including deleted and endpoint-hidden records.
    #[wasm_bindgen(js_name = "edgeRecord")]
    pub fn edge_record(&self, id: &JsGraphEdgeId) -> JsResult<JsGraphEdgeRecordOrUndefined> {
        Ok(match self.handler.edge_record(edge_arg(id)?) {
            Some(edge) => to_js(&EdgeRecord::from(&edge))?,
            None => JsValue::UNDEFINED,
        }
        .into())
    }

    #[wasm_bindgen(js_name = "nodeRecords")]
    pub fn node_records(&self) -> JsResult<JsGraphNodeRecords> {
        Ok(to_js(
            &self
                .handler
                .node_records()
                .iter()
                .map(NodeRecord::from)
                .collect::<Vec<_>>(),
        )?
        .into())
    }

    #[wasm_bindgen(js_name = "edgeRecords")]
    pub fn edge_records(&self) -> JsResult<JsGraphEdgeRecords> {
        Ok(to_js(
            &self
                .handler
                .edge_records()
                .iter()
                .map(EdgeRecord::from)
                .collect::<Vec<_>>(),
        )?
        .into())
    }

    #[wasm_bindgen(js_name = "incomingEdges")]
    pub fn incoming_edges(&self, id: &JsGraphNodeId) -> JsResult<JsGraphEdgeIds> {
        Ok(id_array(self.handler.incoming_edges(node_arg(id)?)?).into())
    }

    #[wasm_bindgen(js_name = "outgoingEdges")]
    pub fn outgoing_edges(&self, id: &JsGraphNodeId) -> JsResult<JsGraphEdgeIds> {
        Ok(id_array(self.handler.outgoing_edges(node_arg(id)?)?).into())
    }

    /// Unique incoming neighbors, sorted by native ID order.
    pub fn predecessors(&self, id: &JsGraphNodeId) -> JsResult<JsGraphNodeIds> {
        Ok(id_array(self.handler.predecessors(node_arg(id)?)?).into())
    }

    /// Unique outgoing neighbors, sorted by native ID order.
    pub fn successors(&self, id: &JsGraphNodeId) -> JsResult<JsGraphNodeIds> {
        Ok(id_array(self.handler.successors(node_arg(id)?)?).into())
    }

    /// Outgoing breadth-first traversal with a visited set, including the start node.
    /// Both limits are required nonnegative u32 integers; zero maxNodes returns no nodes.
    pub fn traverse(
        &self,
        start: &JsGraphNodeId,
        max_depth: f64,
        max_nodes: f64,
    ) -> JsResult<JsGraphNodeIds> {
        let limit = |value: f64| -> JsResult<usize> {
            if !value.is_finite() || value < 0.0 || value > u32::MAX as f64 || value.fract() != 0.0
            {
                return Err(JsValue::from_str(
                    "Graph traversal limits must be nonnegative u32 integers",
                ));
            }
            Ok(value as usize)
        };
        Ok(id_array(self.handler.traverse(
            node_arg(start)?,
            limit(max_depth)?,
            limit(max_nodes)?,
        )?)
        .into())
    }

    /// Stable associated metadata, also accessible while the object is deleted.
    #[wasm_bindgen(js_name = "nodeMeta")]
    pub fn node_meta(&self, id: &JsGraphNodeId) -> JsResult<LoroMap> {
        Ok(LoroMap {
            handler: self.handler.node_meta(node_arg(id)?)?,
        })
    }

    #[wasm_bindgen(js_name = "edgeMeta")]
    pub fn edge_meta(&self, id: &JsGraphEdgeId) -> JsResult<LoroMap> {
        Ok(LoroMap {
            handler: self.handler.edge_meta(edge_arg(id)?)?,
        })
    }

    #[wasm_bindgen(js_name = "isAttached")]
    pub fn is_attached(&self) -> bool {
        self.handler.is_attached()
    }

    #[wasm_bindgen(js_name = "isDeleted")]
    pub fn is_deleted(&self) -> bool {
        self.handler.is_deleted()
    }

    pub fn parent(&self) -> JsResult<JsContainerOrUndefined> {
        match HandlerTrait::parent(&self.handler) {
            Some(parent) => Ok(handler_to_js_value(parent, false)?.into()),
            None => Ok(JsValue::UNDEFINED.into()),
        }
    }

    #[wasm_bindgen(js_name = "getAttached")]
    pub fn get_attached(&self) -> JsResult<JsGraphOrUndefined> {
        if self.is_attached() {
            return Ok(JsValue::from(self.clone()).into());
        }
        match self.handler.get_attached() {
            Some(handler) => Ok(handler_to_js_value(Handler::Graph(handler), false)?.into()),
            None => Ok(JsValue::UNDEFINED.into()),
        }
    }

    /// Observe committed topology and metadata changes. Returns an unsubscribe function.
    #[wasm_bindgen(skip_typescript)]
    pub fn subscribe(&self, callback: js_sys::Function) -> JsResult<JsValue> {
        let observer = observer::Observer::new(callback);
        let doc = self
            .handler
            .doc()
            .ok_or_else(|| JsError::new("Document is not attached"))?;
        let subscription = doc.subscribe(
            &self.handler.id(),
            Arc::new(move |event| {
                put_event_in_pending_queue(observer.clone(), event);
            }),
        );
        Ok(subscription_to_js_function_callback(subscription))
    }

    /// A flat visible node/edge table; metadata contains container IDs.
    #[wasm_bindgen(js_name = "getShallowValue")]
    pub fn get_shallow_value(&self) -> JsGraphShallowValue {
        let value: JsValue = self.handler.get_value().into();
        value.into()
    }

    /// Resolve metadata recursively, without following topology edges.
    #[wasm_bindgen(js_name = "toJSON")]
    pub fn to_json(&self) -> JsGraphValue {
        let value: JsValue = self.handler.get_deep_value().into();
        value.into()
    }

    /// Capture the committed DocState. Pending edits must be committed explicitly.
    pub fn snapshot(&self) -> JsResult<GraphSnapshot> {
        Ok(GraphSnapshot {
            inner: self.handler.snapshot().map_err(repair_error)?,
        })
    }

    /// Apply concrete edge deletions after an atomic native version check.
    /// Commit explicitly to choose an origin/message and publish events.
    #[wasm_bindgen(js_name = "applyRepair")]
    pub fn apply_repair(&self, plan: &GraphRepairPlan) -> JsResult<usize> {
        graph::apply_repair(&self.handler, &plan.inner).map_err(repair_error)
    }
}

/// An immutable native snapshot. Reading or analyzing it does not commit or edit.
#[wasm_bindgen]
pub struct GraphSnapshot {
    inner: graph::GraphSnapshot,
}

#[wasm_bindgen]
impl GraphSnapshot {
    #[wasm_bindgen(getter)]
    pub fn graph(&self) -> JsContainerID {
        JsValue::from(self.inner.graph()).into()
    }

    #[wasm_bindgen(getter)]
    pub fn version(&self) -> JsResult<JsIDs> {
        frontiers_to_ids(self.inner.version())
    }

    #[wasm_bindgen(getter)]
    pub fn nodes(&self) -> JsGraphNodeIds {
        id_array(self.inner.nodes()).into()
    }

    #[wasm_bindgen(getter)]
    pub fn edges(&self) -> JsResult<JsGraphSnapshotEdges> {
        #[derive(Serialize)]
        struct Edge {
            id: String,
            source: String,
            target: String,
        }
        Ok(to_js(
            &self
                .inner
                .edges()
                .iter()
                .map(|edge| Edge {
                    id: edge.id.to_string(),
                    source: edge.source.to_string(),
                    target: edge.target.to_string(),
                })
                .collect::<Vec<_>>(),
        )?
        .into())
    }

    #[wasm_bindgen(js_name = "toJSON")]
    pub fn to_json(&self) -> JsResult<JsGraphSnapshotValue> {
        let value = Object::new();
        Reflect::set(&value, &"graph".into(), &self.graph())?;
        Reflect::set(&value, &"version".into(), &JsValue::from(self.version()?))?;
        Reflect::set(&value, &"nodes".into(), &self.nodes())?;
        Reflect::set(&value, &"edges".into(), &JsValue::from(self.edges()?))?;
        Ok(JsValue::from(value).into())
    }

    /// Analyze all visible edges, or exactly the supplied subset. Does not hide cycles.
    #[wasm_bindgen(js_name = "analyzeCycles")]
    pub fn analyze_cycles(
        &self,
        selected_edges: Option<JsGraphEdgeIds>,
    ) -> JsResult<JsCycleReport> {
        let report =
            graph::analyze_cycles(&self.inner, &scope(selected_edges)?).map_err(repair_error)?;
        let value = Object::new();
        let components = Array::from_iter(report.components.iter().map(id_array));
        let cyclic = Array::new();
        for component in &report.cyclic_components {
            let entry = Object::new();
            Reflect::set(&entry, &"nodes".into(), &id_array(&component.nodes))?;
            Reflect::set(&entry, &"witness".into(), &id_array(&component.witness))?;
            cyclic.push(&entry);
        }
        Reflect::set(&value, &"components".into(), &components)?;
        Reflect::set(&value, &"cyclicComponents".into(), &cyclic)?;
        Reflect::set(&value, &"selfLoops".into(), &id_array(&report.self_loops))?;
        Reflect::set(&value, &"scope".into(), &scope_value(&report.scope)?)?;
        Reflect::set(&value, &"isAcyclic".into(), &report.is_acyclic().into())?;
        Ok(JsValue::from(value).into())
    }

    /// Plan deterministic deletions; this policy does not minimize deletion count.
    #[wasm_bindgen(js_name = "planBreakCycles")]
    pub fn plan_break_cycles(
        &self,
        selected_edges: Option<JsGraphEdgeIds>,
        policy: Option<JsGraphRepairPolicy>,
    ) -> JsResult<GraphRepairPlan> {
        if policy
            .is_some_and(|policy| policy.as_string().as_deref() != Some("ascending-node-id-v1"))
        {
            return Err(JsValue::from_str("Unknown graph repair policy"));
        }
        let inner = graph::plan_break_cycles(
            &self.inner,
            &scope(selected_edges)?,
            RepairPolicy::AscendingNodeIdV1,
        )
        .map_err(repair_error)?;
        Ok(GraphRepairPlan { inner })
    }
}

/// An opaque native plan. JSON copies can be inspected but cannot be applied.
#[wasm_bindgen]
pub struct GraphRepairPlan {
    inner: graph::RepairPlan,
}

#[wasm_bindgen]
impl GraphRepairPlan {
    #[wasm_bindgen(getter)]
    pub fn graph(&self) -> JsContainerID {
        JsValue::from(self.inner.graph()).into()
    }

    #[wasm_bindgen(getter)]
    pub fn version(&self) -> JsResult<JsIDs> {
        frontiers_to_ids(self.inner.version())
    }

    #[wasm_bindgen(js_name = "isEmpty")]
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    #[wasm_bindgen(js_name = "toJSON")]
    pub fn to_json(&self) -> JsResult<JsGraphRepairPlanValue> {
        let value = Object::new();
        Reflect::set(&value, &"graph".into(), &self.graph())?;
        Reflect::set(&value, &"version".into(), &JsValue::from(self.version()?))?;
        let policy = Object::new();
        Reflect::set(&policy, &"id".into(), &self.inner.policy().id().into())?;
        Reflect::set(
            &policy,
            &"version".into(),
            &self.inner.policy().version().into(),
        )?;
        Reflect::set(&value, &"policy".into(), &policy)?;
        Reflect::set(&value, &"scope".into(), &scope_value(self.inner.scope())?)?;
        let deletions = Array::new();
        for deletion in self.inner.deletions() {
            let entry = Object::new();
            let reason = Object::new();
            match &deletion.reason {
                RepairReason::SelfLoop => {
                    Reflect::set(&reason, &"type".into(), &"selfLoop".into())?;
                }
                RepairReason::DescendingEdgeInComponent { component } => {
                    Reflect::set(&reason, &"type".into(), &"descendingEdgeInComponent".into())?;
                    Reflect::set(&reason, &"component".into(), &(*component as f64).into())?;
                }
            }
            Reflect::set(&entry, &"edge".into(), &deletion.edge.to_string().into())?;
            Reflect::set(&entry, &"reason".into(), &reason)?;
            deletions.push(&entry);
        }
        Reflect::set(&value, &"deletions".into(), &deletions)?;
        Ok(JsValue::from(value).into())
    }
}
