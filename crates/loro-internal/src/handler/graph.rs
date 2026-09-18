use super::{
    create_handler, BasicHandler, DetachedInner, Handler, HandlerTrait, MapHandler, MaybeDetached,
};
pub use crate::container::graph::{GraphDiff, GraphEdge, GraphNode};
use crate::{
    container::{
        graph::{GraphChange, GraphOp},
        idx::ContainerIdx,
    },
    graph::{GraphRepairError, GraphSnapshot, GraphSnapshotEdge},
    state::graph_state::GraphState,
    txn::{EventHint, Transaction},
    version::Frontiers,
};
use loro_common::{
    ContainerID, ContainerType, GraphEdgeId, GraphNodeId, LoroError, LoroResult, LoroValue, ID,
};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::Arc,
};

#[derive(Clone)]
pub struct GraphHandler {
    pub(super) inner: MaybeDetached<GraphInner>,
}
pub(super) struct GraphInner {
    state: GraphState,
    next: ID,
    maps: BTreeMap<ID, MapHandler>,
}
impl std::fmt::Debug for GraphHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GraphHandler")
            .field("id", &self.id())
            .finish()
    }
}
impl GraphHandler {
    pub fn new_detached() -> Self {
        use crate::configure::{DefaultRandom, SecureRandomGenerator};
        Self {
            inner: MaybeDetached::Detached(Arc::new(crate::sync::Mutex::new(DetachedInner::new(
                GraphInner {
                    state: GraphState::new(ContainerIdx::from_index_and_type(
                        0,
                        ContainerType::Graph,
                    )),
                    next: ID::new(DefaultRandom.next_u64(), 0),
                    maps: BTreeMap::new(),
                },
            )))),
        }
    }
    fn read<R>(&self, f: impl FnOnce(&GraphState) -> R) -> R {
        match &self.inner {
            MaybeDetached::Detached(d) => f(&d.lock().value.state),
            MaybeDetached::Attached(a) => a.with_state(|s| f(s.as_graph_state().unwrap())),
        }
    }
    fn write(&self, f: impl FnOnce(ID, &GraphState) -> LoroResult<GraphOp>) -> LoroResult<ID> {
        match &self.inner {
            MaybeDetached::Detached(d) => {
                let mut d = d.lock();
                let g = &mut d.value;
                let id = g.next;
                let op = f(id, &g.state)?;
                let change = GraphChange {
                    id,
                    op: op.clone(),
                    forward: true,
                };
                g.state.validate_changes(std::slice::from_ref(&change))?;
                g.state.apply_changes(vec![change]);
                if op.created_meta().is_some() {
                    g.maps.insert(id, MapHandler::new_detached());
                }
                g.next = g.next.inc(1);
                Ok(id)
            }
            MaybeDetached::Attached(a) => a.with_txn(|txn| {
                let id = txn.next_id();
                let op = a.with_state(|s| f(id, s.as_graph_state().unwrap()))?;
                self.write_with_txn(txn, op)?;
                Ok(id)
            }),
        }
    }
    fn write_with_txn(&self, txn: &mut Transaction, op: GraphOp) -> LoroResult<()> {
        let a = self.inner.try_attached_state()?;
        let c = GraphChange {
            id: txn.next_id(),
            op: op.clone(),
            forward: true,
        };
        let hint = a.with_state(|s| s.as_graph_state().unwrap().preview(c))?;
        txn.apply_local_op(
            a.container_idx,
            crate::op::RawOpContent::Graph(Arc::new(op)),
            EventHint::Graph(hint),
            &a.doc,
        )
    }
    pub fn create_node(&self) -> LoroResult<GraphNodeId> {
        self.write(|id, _| {
            Ok(GraphOp::CreateNode {
                id: GraphNodeId::from_id(id),
            })
        })
        .map(GraphNodeId::from_id)
    }
    pub fn create_edge(&self, source: GraphNodeId, target: GraphNodeId) -> LoroResult<GraphEdgeId> {
        self.write(|id, _| {
            Ok(GraphOp::CreateEdge {
                id: GraphEdgeId::from_id(id),
                source,
                target,
            })
        })
        .map(GraphEdgeId::from_id)
    }
    pub fn delete_node(&self, id: GraphNodeId) -> LoroResult<()> {
        self.write(|_, _| Ok(GraphOp::DeleteNode { id }))
            .map(|_| ())
    }
    pub fn delete_edge(&self, id: GraphEdgeId) -> LoroResult<()> {
        self.write(|_, _| Ok(GraphOp::DeleteEdge { id }))
            .map(|_| ())
    }
    pub fn restore_node(&self, id: GraphNodeId) -> LoroResult<()> {
        self.write(|_, s| {
            let n = s.node_record(id).ok_or_else(missing)?;
            Ok(GraphOp::RestoreNode {
                id,
                deletes: n.delete_tags,
            })
        })
        .map(|_| ())
    }
    pub fn restore_edge(&self, id: GraphEdgeId) -> LoroResult<()> {
        self.write(|_, s| {
            let e = s.edge_record(id).ok_or_else(missing)?;
            Ok(GraphOp::RestoreEdge {
                id,
                deletes: e.delete_tags,
            })
        })
        .map(|_| ())
    }
    pub fn nodes(&self) -> Vec<GraphNodeId> {
        self.read(GraphState::nodes)
    }
    pub fn edges(&self) -> Vec<GraphEdgeId> {
        self.read(GraphState::edges)
    }
    pub fn node_count(&self) -> usize {
        self.read(GraphState::node_count)
    }
    pub fn edge_count(&self) -> usize {
        self.read(GraphState::edge_count)
    }
    pub fn node_record(&self, id: GraphNodeId) -> Option<GraphNode> {
        self.read(|s| s.node_record(id))
    }
    pub fn edge_record(&self, id: GraphEdgeId) -> Option<GraphEdge> {
        self.read(|s| s.edge_record(id))
    }
    pub fn get_node(&self, id: GraphNodeId) -> Option<GraphNode> {
        self.node_record(id).filter(|n| n.visible)
    }
    pub fn get_edge(&self, id: GraphEdgeId) -> Option<GraphEdge> {
        self.edge_record(id).filter(|e| e.visible)
    }
    pub fn node_records(&self) -> Vec<GraphNode> {
        self.read(GraphState::all_nodes)
    }
    pub fn edge_records(&self) -> Vec<GraphEdge> {
        self.read(GraphState::all_edges)
    }
    pub fn incoming_edges(&self, id: GraphNodeId) -> LoroResult<Vec<GraphEdgeId>> {
        self.read(|s| s.adjacent(id, true))
    }
    pub fn outgoing_edges(&self, id: GraphNodeId) -> LoroResult<Vec<GraphEdgeId>> {
        self.read(|s| s.adjacent(id, false))
    }
    pub fn predecessors(&self, id: GraphNodeId) -> LoroResult<Vec<GraphNodeId>> {
        self.neighbors(id, true)
    }
    pub fn successors(&self, id: GraphNodeId) -> LoroResult<Vec<GraphNodeId>> {
        self.neighbors(id, false)
    }
    fn neighbors(&self, id: GraphNodeId, incoming: bool) -> LoroResult<Vec<GraphNodeId>> {
        self.read(|s| {
            Ok(s.adjacent(id, incoming)?
                .into_iter()
                .map(|e| {
                    let e = s.edge_record(e).unwrap();
                    if incoming {
                        e.source
                    } else {
                        e.target
                    }
                })
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect())
        })
    }
    /// Breadth-first outgoing traversal, including the starting node, bounded in depth and count.
    pub fn traverse(
        &self,
        start: GraphNodeId,
        max_depth: usize,
        max_nodes: usize,
    ) -> LoroResult<Vec<GraphNodeId>> {
        self.read(|s| {
            if s.node_record(start).filter(|n| n.visible).is_none() {
                return Err(missing());
            }
            let mut seen = BTreeSet::new();
            let mut q = VecDeque::from([(start, 0)]);
            let mut result = Vec::new();
            while let Some((n, d)) = q.pop_front() {
                if result.len() == max_nodes {
                    break;
                }
                if !seen.insert(n) {
                    continue;
                }
                result.push(n);
                if d < max_depth {
                    for e in s.adjacent(n, false)? {
                        let e = s.edge_record(e).unwrap();
                        if !seen.contains(&e.target) {
                            q.push_back((e.target, d + 1));
                        }
                    }
                }
            }
            Ok(result)
        })
    }
    pub fn node_meta(&self, id: GraphNodeId) -> LoroResult<MapHandler> {
        if self.node_record(id).is_none() {
            return Err(missing());
        }
        self.meta(id.id())
    }
    pub fn edge_meta(&self, id: GraphEdgeId) -> LoroResult<MapHandler> {
        if self.edge_record(id).is_none() {
            return Err(missing());
        }
        self.meta(id.id())
    }
    fn meta(&self, id: ID) -> LoroResult<MapHandler> {
        match &self.inner {
            MaybeDetached::Detached(d) => d.lock().value.maps.get(&id).cloned().ok_or_else(missing),
            MaybeDetached::Attached(a) => Ok(create_handler(
                a,
                ContainerID::new_normal(id, ContainerType::Map),
            )
            .into_map()
            .unwrap()),
        }
    }
    pub fn snapshot(&self) -> Result<GraphSnapshot, GraphRepairError> {
        let a = self
            .attached_handler()
            .ok_or(GraphRepairError::DetachedContainer)?;
        let txn = a.doc.txn.lock();
        if txn.as_ref().is_some_and(|t| !t.is_empty()) {
            return Err(GraphRepairError::UncommittedChanges);
        }
        let mut state = a.doc.state.lock();
        let version = state.frontiers.clone();
        Ok(state.with_state_mut(a.container_idx, |s| {
            let s = s.as_graph_state().unwrap();
            let edges = s
                .edges()
                .into_iter()
                .map(|id| {
                    let e = s.edge_record(id).unwrap();
                    GraphSnapshotEdge {
                        id,
                        source: e.source,
                        target: e.target,
                    }
                })
                .collect();
            GraphSnapshot::new(a.id.clone(), version, s.nodes(), edges)
        }))
    }
    pub fn delete_edges_if_version(
        &self,
        expected: &Frontiers,
        edges: &[GraphEdgeId],
    ) -> Result<usize, GraphRepairError> {
        let a = self
            .attached_handler()
            .ok_or(GraphRepairError::DetachedContainer)?;
        let mut guard = a.doc.txn.lock();
        if !a.doc.can_edit() {
            return Err(LoroError::EditWhenDetached.into());
        }
        if guard.as_ref().is_some_and(|t| !t.is_empty()) {
            return Err(GraphRepairError::UncommittedChanges);
        }
        let edges: BTreeSet<_> = edges.iter().copied().collect();
        {
            let mut state = a.doc.state.lock();
            if &state.frontiers != expected {
                return Err(GraphRepairError::StalePlan);
            }
            if state.is_deleted(a.container_idx) {
                return Err(LoroError::ContainerDeleted {
                    container: Box::new(a.id.clone()),
                }
                .into());
            }
            state.with_state_mut(a.container_idx, |s| {
                let s = s.as_graph_state().unwrap();
                for e in &edges {
                    if !s.edge_record(*e).is_some_and(|e| e.visible) {
                        return Err(GraphRepairError::InvalidSelection(*e));
                    }
                }
                Ok(())
            })?;
        }
        if edges.is_empty() {
            return Ok(0);
        }
        if guard.is_none() {
            *guard = Some(a.doc.txn()?);
        }
        let txn = guard.as_mut().unwrap();
        for id in &edges {
            self.write_with_txn(txn, GraphOp::DeleteEdge { id: *id })?;
        }
        Ok(edges.len())
    }
    pub fn clear(&self) -> LoroResult<()> {
        for id in self.nodes() {
            self.delete_node(id)?;
        }
        Ok(())
    }
    pub fn is_deleted(&self) -> bool {
        self.attached_handler()
            .is_some_and(BasicHandler::is_deleted)
    }
    pub(crate) fn apply_delta(
        &self,
        diff: GraphDiff,
        remap: &mut rustc_hash::FxHashMap<ContainerID, ContainerID>,
    ) -> LoroResult<()> {
        fn remapped(id: ID, m: &rustc_hash::FxHashMap<ContainerID, ContainerID>) -> ID {
            let mut c = ContainerID::new_normal(id, ContainerType::Map);
            while let Some(next) = m.get(&c) {
                c = next.clone();
            }
            match c {
                ContainerID::Normal { peer, counter, .. } => ID::new(peer, counter),
                _ => id,
            }
        }
        // A history diff names original operation IDs. Compute its net effect
        // first; replaying each inverse separately can create a new Delete that
        // the following Restore cannot name. Preserve untouched remote tags.
        let plan = self.read(|state| state.local_diff_ops(&diff.ops, |id| remapped(id, remap)))?;
        for op in plan {
            let op = match op {
                GraphOp::CreateNode { id } => {
                    let new = self.create_node()?;
                    remap.insert(
                        id.associated_meta_container(),
                        new.associated_meta_container(),
                    );
                    continue;
                }
                GraphOp::CreateEdge { id, source, target } => {
                    let new = self.create_edge(
                        GraphNodeId::from_id(remapped(source.id(), remap)),
                        GraphNodeId::from_id(remapped(target.id(), remap)),
                    )?;
                    remap.insert(
                        id.associated_meta_container(),
                        new.associated_meta_container(),
                    );
                    continue;
                }
                GraphOp::DeleteNode { id } => GraphOp::DeleteNode {
                    id: GraphNodeId::from_id(remapped(id.id(), remap)),
                },
                GraphOp::DeleteEdge { id } => GraphOp::DeleteEdge {
                    id: GraphEdgeId::from_id(remapped(id.id(), remap)),
                },
                GraphOp::RestoreNode { id, deletes } => GraphOp::RestoreNode {
                    id: GraphNodeId::from_id(remapped(id.id(), remap)),
                    deletes,
                },
                GraphOp::RestoreEdge { id, deletes } => GraphOp::RestoreEdge {
                    id: GraphEdgeId::from_id(remapped(id.id(), remap)),
                    deletes,
                },
            };
            self.write(|_, _| Ok(op))?;
        }
        Ok(())
    }
}
fn missing() -> LoroError {
    LoroError::ArgErr(
        "Graph object does not belong to this graph or does not exist at this version".into(),
    )
}
impl HandlerTrait for GraphHandler {
    fn is_attached(&self) -> bool {
        self.inner.is_attached()
    }
    fn attached_handler(&self) -> Option<&BasicHandler> {
        self.inner.attached_handler()
    }
    fn get_value(&self) -> LoroValue {
        self.read(GraphState::value)
    }
    fn get_deep_value(&self) -> LoroValue {
        let mut value = self.get_value();
        if let LoroValue::Map(m) = &mut value {
            for table in m.make_mut().values_mut() {
                if let LoroValue::List(l) = table {
                    for row in l.make_mut() {
                        if let LoroValue::Map(m) = row {
                            if let Some(LoroValue::Container(cid)) = m.get("meta") {
                                let id = match cid {
                                    ContainerID::Normal { peer, counter, .. } => {
                                        ID::new(*peer, *counter)
                                    }
                                    _ => unreachable!(),
                                };
                                m.make_mut()
                                    .insert("meta".into(), self.meta(id).unwrap().get_deep_value());
                            }
                        }
                    }
                }
            }
        }
        value
    }
    fn kind(&self) -> ContainerType {
        ContainerType::Graph
    }
    fn to_handler(&self) -> Handler {
        Handler::Graph(self.clone())
    }
    fn from_handler(h: Handler) -> Option<Self> {
        h.into_graph().ok()
    }
    fn doc(&self) -> Option<crate::LoroDoc> {
        self.attached_handler().map(|a| a.doc.clone())
    }
    fn get_attached(&self) -> Option<Self> {
        match &self.inner {
            MaybeDetached::Attached(a) => Some(Self {
                inner: a.clone().into(),
            }),
            MaybeDetached::Detached(d) => {
                d.lock().attached.clone().map(|a| Self { inner: a.into() })
            }
        }
    }
    fn attach(
        &self,
        txn: &mut Transaction,
        parent: &BasicHandler,
        self_id: ContainerID,
    ) -> LoroResult<Self> {
        let target = create_handler(parent, self_id).into_graph().unwrap();
        let mut remap = BTreeMap::new();
        for n in self.node_records() {
            let id = GraphNodeId::from_id(txn.next_id());
            target.write_with_txn(txn, GraphOp::CreateNode { id })?;
            self.node_meta(n.id)?.attach(
                txn,
                target.attached_handler().unwrap(),
                id.associated_meta_container(),
            )?;
            remap.insert(n.id, id);
        }
        for e in self.edge_records() {
            let id = GraphEdgeId::from_id(txn.next_id());
            target.write_with_txn(
                txn,
                GraphOp::CreateEdge {
                    id,
                    source: remap[&e.source],
                    target: remap[&e.target],
                },
            )?;
            self.edge_meta(e.id)?.attach(
                txn,
                target.attached_handler().unwrap(),
                id.associated_meta_container(),
            )?;
            if !e.alive {
                target.write_with_txn(txn, GraphOp::DeleteEdge { id })?;
            }
        }
        for n in self.node_records() {
            if !n.alive {
                target.write_with_txn(txn, GraphOp::DeleteNode { id: remap[&n.id] })?;
            }
        }
        if let MaybeDetached::Detached(d) = &self.inner {
            d.lock().attached = target.attached_handler().cloned();
        }
        Ok(target)
    }
}
