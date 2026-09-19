//! Incremental remove-wins graph state. Only incident edges are re-evaluated on node changes.
mod local_diff;
pub(crate) mod order_index;
use order_index::OrderIndex;

use super::{ApplyLocalOpReturn, ContainerState, DiffApplyContext, FastStateSnapshot};
use crate::{
    container::{
        graph::{
            GraphChange, GraphDiff, GraphEdge, GraphNode, GraphOp, GraphOrderDelta,
            GraphOrderValue, GraphPosition, OrderedGraphEdge,
        },
        idx::ContainerIdx,
    },
    event::{Diff, Index, InternalDiff},
    op::{Op, RawOp, RawOpContent},
    LoroDocInner,
};
use loro_common::{
    ContainerID, ContainerType, GraphEdgeId, GraphNodeId, IdFull, IdLp, LoroError, LoroResult,
    LoroValue, TreeID, ID,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Weak,
};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Life {
    created_at: u32,
    deletes: BTreeMap<ID, u32>,
    // Retain restore identities so history retreat can remove exactly one restore.
    restores: BTreeMap<ID, (u32, Vec<ID>)>,
    #[serde(skip)]
    removed: BTreeMap<ID, usize>,
}
impl Life {
    fn active(&self) -> Vec<ID> {
        self.deletes
            .keys()
            .filter(|id| !self.removed.contains_key(id))
            .copied()
            .collect()
    }
    fn alive(&self) -> bool {
        self.deletes.keys().all(|id| self.removed.contains_key(id))
    }
    fn delete(&mut self, id: ID, lamport: u32, forward: bool) {
        if forward {
            self.deletes.insert(id, lamport);
        } else {
            self.deletes.remove(&id);
        }
    }
    fn restore(&mut self, id: ID, lamport: u32, deletes: &[ID], forward: bool) {
        if forward {
            if self
                .restores
                .insert(id, (lamport, deletes.to_vec()))
                .is_none()
            {
                for tag in deletes {
                    *self.removed.entry(*tag).or_default() += 1;
                }
            }
        } else if let Some((_, tags)) = self.restores.remove(&id) {
            for tag in tags {
                let n = self.removed.get_mut(&tag).expect("restore tag count");
                *n -= 1;
                if *n == 0 {
                    self.removed.remove(&tag);
                }
            }
        }
    }
    fn rebuild(&mut self) {
        self.removed.clear();
        for (_, tags) in self.restores.values() {
            for tag in tags {
                *self.removed.entry(*tag).or_default() += 1;
            }
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Edge {
    source: GraphNodeId,
    target: GraphNodeId,
    life: Life,
    // The order of writes is independent from the order of fractional keys.
    orders: BTreeMap<IdLp, (i32, GraphPosition)>,
}
impl Edge {
    fn order(&self) -> GraphOrderValue {
        let (id, (counter, position)) = self
            .orders
            .last_key_value()
            .expect("edge creation has a position");
        GraphOrderValue {
            position: position.clone(),
            last_order: IdFull::new(id.peer, *counter, id.lamport),
        }
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Records {
    nodes: BTreeMap<GraphNodeId, Life>,
    edges: BTreeMap<GraphEdgeId, Edge>,
}
#[derive(Clone, Debug)]
pub struct GraphState {
    idx: ContainerIdx,
    records: Records,
    incoming: BTreeMap<GraphNodeId, BTreeSet<GraphEdgeId>>,
    outgoing: BTreeMap<GraphNodeId, BTreeSet<GraphEdgeId>>,
    visible_nodes: BTreeSet<GraphNodeId>,
    visible_edges: BTreeSet<GraphEdgeId>,
    pub(crate) ordered_outgoing: BTreeMap<GraphNodeId, OrderIndex>,
    /// Local key-generation policy, never persisted or synchronized.
    pub(crate) order_jitter: u8,
}
fn invalid() -> LoroError {
    LoroError::DecodeError("Invalid graph reference or lifecycle operation".into())
}
impl GraphState {
    pub(crate) fn new(idx: ContainerIdx) -> Self {
        Self {
            idx,
            records: Records::default(),
            incoming: BTreeMap::new(),
            outgoing: BTreeMap::new(),
            visible_nodes: BTreeSet::new(),
            visible_edges: BTreeSet::new(),
            ordered_outgoing: BTreeMap::new(),
            order_jitter: 0,
        }
    }
    pub fn nodes(&self) -> Vec<GraphNodeId> {
        self.visible_nodes.iter().copied().collect()
    }
    pub fn edges(&self) -> Vec<GraphEdgeId> {
        self.visible_edges.iter().copied().collect()
    }
    pub fn node_count(&self) -> usize {
        self.visible_nodes.len()
    }
    pub fn edge_count(&self) -> usize {
        self.visible_edges.len()
    }
    pub fn node_record(&self, id: GraphNodeId) -> Option<GraphNode> {
        let life = self.records.nodes.get(&id)?;
        Some(GraphNode {
            id,
            alive: life.alive(),
            visible: self.visible_nodes.contains(&id),
            delete_tags: life.active(),
        })
    }
    pub fn edge_record(&self, id: GraphEdgeId) -> Option<GraphEdge> {
        let edge = self.records.edges.get(&id)?;
        let order = edge.order();
        Some(GraphEdge {
            id,
            source: edge.source,
            target: edge.target,
            position: order.position,
            last_order: order.last_order,
            alive: edge.life.alive(),
            visible: self.visible_edges.contains(&id),
            delete_tags: edge.life.active(),
        })
    }
    pub fn all_nodes(&self) -> Vec<GraphNode> {
        self.records
            .nodes
            .keys()
            .map(|id| self.node_record(*id).unwrap())
            .collect()
    }
    pub fn all_edges(&self) -> Vec<GraphEdge> {
        self.records
            .edges
            .keys()
            .map(|id| self.edge_record(*id).unwrap())
            .collect()
    }
    pub fn adjacent(&self, id: GraphNodeId, incoming: bool) -> LoroResult<Vec<GraphEdgeId>> {
        if !self.records.nodes.contains_key(&id) {
            return Err(invalid());
        }
        Ok(if incoming {
            &self.incoming
        } else {
            &self.outgoing
        }
        .get(&id)
        .into_iter()
        .flatten()
        .filter(|e| self.visible_edges.contains(e))
        .copied()
        .collect())
    }
    pub fn ordered_out_edges(&self, source: GraphNodeId) -> LoroResult<Vec<OrderedGraphEdge>> {
        if !self.records.nodes.contains_key(&source) {
            return Err(invalid());
        }
        Ok(self
            .ordered_outgoing
            .get(&source)
            .into_iter()
            .flat_map(OrderIndex::iter)
            .map(|id| self.ordered_edge(id))
            .collect())
    }
    pub fn out_edge_at(&self, source: GraphNodeId, index: usize) -> Option<OrderedGraphEdge> {
        self.ordered_outgoing
            .get(&source)?
            .at(index)
            .map(|id| self.ordered_edge(id))
    }
    pub fn index_of_out_edge(&self, id: GraphEdgeId) -> Option<usize> {
        let edge = self.records.edges.get(&id)?;
        self.ordered_outgoing.get(&edge.source)?.rank(id)
    }
    fn ordered_edge(&self, id: GraphEdgeId) -> OrderedGraphEdge {
        let edge = &self.records.edges[&id];
        OrderedGraphEdge {
            edge_id: id,
            target: edge.target,
            position: edge.order().position,
        }
    }
    fn incident(&self, id: GraphNodeId) -> BTreeSet<GraphEdgeId> {
        self.incoming
            .get(&id)
            .into_iter()
            .flatten()
            .chain(self.outgoing.get(&id).into_iter().flatten())
            .copied()
            .collect()
    }
    fn update_edge(&mut self, id: GraphEdgeId) {
        let edge = self.records.edges.get(&id);
        let visible = edge.is_some_and(|e| {
            e.life.alive()
                && self.visible_nodes.contains(&e.source)
                && self.visible_nodes.contains(&e.target)
        });
        if let Some(edge) = edge {
            if let Some(index) = self.ordered_outgoing.get_mut(&edge.source) {
                index.remove(id);
            }
            if visible {
                self.ordered_outgoing
                    .entry(edge.source)
                    .or_default()
                    .insert(edge.order().position, id);
            }
        }
        if visible {
            self.visible_edges.insert(id);
        } else {
            self.visible_edges.remove(&id);
        }
    }
    fn update_node(&mut self, id: GraphNodeId) {
        if self.records.nodes.get(&id).is_some_and(Life::alive) {
            self.visible_nodes.insert(id);
        } else {
            self.visible_nodes.remove(&id);
        }
        for e in self.incident(id) {
            self.update_edge(e);
        }
    }
    pub(crate) fn validate_changes(&self, changes: &[GraphChange]) -> LoroResult<()> {
        // Track only new identities in this batch; no whole-graph cloning on the import hot path.
        let mut nodes = BTreeSet::new();
        let mut edges = BTreeSet::new();
        let mut deletes: BTreeMap<ID, ID> = BTreeMap::new();
        for c in changes {
            c.op.validate(c.id)?;
            if !c.forward {
                continue;
            }
            match &c.op {
                GraphOp::CreateNode { id } => {
                    if self.records.nodes.contains_key(id)
                        || self
                            .records
                            .edges
                            .contains_key(&GraphEdgeId::from_id(id.id()))
                        || edges.contains(&GraphEdgeId::from_id(id.id()))
                        || !nodes.insert(*id)
                    {
                        return Err(invalid());
                    }
                }
                GraphOp::CreateEdge {
                    id, source, target, ..
                } => {
                    if self.records.edges.contains_key(id)
                        || self
                            .records
                            .nodes
                            .contains_key(&GraphNodeId::from_id(id.id()))
                        || nodes.contains(&GraphNodeId::from_id(id.id()))
                        || !edges.insert(*id)
                    {
                        return Err(invalid());
                    }
                    for n in [source, target] {
                        if !self.records.nodes.contains_key(n) && !nodes.contains(n) {
                            return Err(invalid());
                        }
                    }
                }
                GraphOp::DeleteNode { id } | GraphOp::RestoreNode { id, .. } => {
                    if !self.records.nodes.contains_key(id) && !nodes.contains(id) {
                        return Err(invalid());
                    }
                }
                GraphOp::DeleteEdge { id }
                | GraphOp::RestoreEdge { id, .. }
                | GraphOp::SetEdgeOrder { id, .. } => {
                    if !self.records.edges.contains_key(id) && !edges.contains(id) {
                        return Err(invalid());
                    }
                }
            }
            match &c.op {
                GraphOp::DeleteNode { id } => {
                    deletes.insert(c.id, id.id());
                }
                GraphOp::DeleteEdge { id } => {
                    deletes.insert(c.id, id.id());
                }
                GraphOp::RestoreNode { id, deletes: tags } => {
                    for tag in tags {
                        if deletes.get(tag) != Some(&id.id())
                            && !self
                                .records
                                .nodes
                                .get(id)
                                .is_some_and(|n| n.deletes.contains_key(tag))
                        {
                            return Err(invalid());
                        }
                    }
                }
                GraphOp::RestoreEdge { id, deletes: tags } => {
                    for tag in tags {
                        if deletes.get(tag) != Some(&id.id())
                            && !self
                                .records
                                .edges
                                .get(id)
                                .is_some_and(|e| e.life.deletes.contains_key(tag))
                        {
                            return Err(invalid());
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }
    pub(crate) fn apply_changes(&mut self, changes: Vec<GraphChange>) -> GraphDiff {
        let mut result = GraphDiff::default();
        for c in changes {
            let mut ns = BTreeSet::new();
            let mut es = BTreeSet::new();
            let order_before = match &c.op {
                GraphOp::CreateEdge { id, source, .. } => {
                    Some((*id, *source, self.records.edges.get(id).map(Edge::order)))
                }
                GraphOp::SetEdgeOrder { id, .. } => {
                    let edge = self.records.edges.get(id).expect("validated edge");
                    Some((*id, edge.source, Some(edge.order())))
                }
                _ => None,
            };
            match &c.op {
                GraphOp::CreateNode { id } => {
                    ns.insert(*id);
                    if c.forward {
                        self.records.nodes.entry(*id).or_insert_with(|| Life {
                            created_at: c.lamport,
                            ..Default::default()
                        });
                    } else {
                        self.records.nodes.remove(id);
                    }
                }
                GraphOp::CreateEdge {
                    id,
                    source,
                    target,
                    position,
                } => {
                    es.insert(*id);
                    if c.forward {
                        self.records.edges.insert(
                            *id,
                            Edge {
                                source: *source,
                                target: *target,
                                life: Life {
                                    created_at: c.lamport,
                                    ..Default::default()
                                },
                                orders: BTreeMap::from([(
                                    IdLp::new(c.id.peer, c.lamport),
                                    (c.id.counter, position.clone()),
                                )]),
                            },
                        );
                        self.outgoing.entry(*source).or_default().insert(*id);
                        self.incoming.entry(*target).or_default().insert(*id);
                    } else {
                        if let Some(index) = self.ordered_outgoing.get_mut(source) {
                            index.remove(*id);
                        }
                        self.records.edges.remove(id);
                        if let Some(s) = self.outgoing.get_mut(source) {
                            s.remove(id);
                        }
                        if let Some(s) = self.incoming.get_mut(target) {
                            s.remove(id);
                        }
                    }
                }
                GraphOp::DeleteNode { id } => {
                    ns.insert(*id);
                    self.records
                        .nodes
                        .get_mut(id)
                        .expect("validated node")
                        .delete(c.id, c.lamport, c.forward);
                }
                GraphOp::DeleteEdge { id } => {
                    es.insert(*id);
                    self.records
                        .edges
                        .get_mut(id)
                        .expect("validated edge")
                        .life
                        .delete(c.id, c.lamport, c.forward);
                }
                GraphOp::RestoreNode { id, deletes } => {
                    ns.insert(*id);
                    self.records
                        .nodes
                        .get_mut(id)
                        .expect("validated node")
                        .restore(c.id, c.lamport, deletes, c.forward);
                }
                GraphOp::RestoreEdge { id, deletes } => {
                    es.insert(*id);
                    self.records
                        .edges
                        .get_mut(id)
                        .expect("validated edge")
                        .life
                        .restore(c.id, c.lamport, deletes, c.forward);
                }
                GraphOp::SetEdgeOrder { id, position } => {
                    es.insert(*id);
                    let orders = &mut self
                        .records
                        .edges
                        .get_mut(id)
                        .expect("validated edge")
                        .orders;
                    let write = IdLp::new(c.id.peer, c.lamport);
                    if c.forward {
                        orders.insert(write, (c.id.counter, position.clone()));
                    } else {
                        orders.remove(&write);
                    }
                }
            }
            if let Some((id, source, before)) = order_before {
                let after = self.records.edges.get(&id).map(Edge::order);
                if before != after {
                    result
                        .orders
                        .entry(id)
                        .and_modify(|delta| delta.after = after.clone())
                        .or_insert(GraphOrderDelta {
                            source,
                            before,
                            after,
                        });
                }
            }
            for id in ns {
                es.extend(self.incident(id));
                self.update_node(id);
                result.nodes.insert(id, self.node_record(id));
            }
            for id in es {
                self.update_edge(id);
                result.edges.insert(id, self.edge_record(id));
            }
            result.ops.push(c);
        }
        result
    }
    pub(crate) fn preview(&self, change: GraphChange) -> LoroResult<GraphDiff> {
        self.validate_changes(std::slice::from_ref(&change))?;
        if let GraphOp::SetEdgeOrder { id, position } = &change.op {
            if change.forward {
                // A local order preview needs the winning register only. Copying
                // the edge's entire history would make repeated drags quadratic.
                let mut edge = self.edge_record(*id).expect("validated edge");
                let before = GraphOrderValue {
                    position: edge.position.clone(),
                    last_order: edge.last_order,
                };
                let mut diff = GraphDiff::default();
                if IdLp::new(change.id.peer, change.lamport) > edge.last_order.idlp() {
                    edge.position = position.clone();
                    edge.last_order =
                        IdFull::new(change.id.peer, change.id.counter, change.lamport);
                    diff.orders.insert(
                        *id,
                        GraphOrderDelta {
                            source: edge.source,
                            before: Some(before),
                            after: Some(GraphOrderValue {
                                position: edge.position.clone(),
                                last_order: edge.last_order,
                            }),
                        },
                    );
                }
                diff.edges.insert(*id, Some(edge));
                diff.ops.push(change);
                return Ok(diff);
            }
        }
        // Event records are computed from the affected object and its adjacency only.
        let mut local = Self::new(self.idx);
        let mut ns = BTreeSet::new();
        let mut es = BTreeSet::new();
        match &change.op {
            GraphOp::CreateNode { id }
            | GraphOp::DeleteNode { id }
            | GraphOp::RestoreNode { id, .. } => {
                ns.insert(*id);
                es.extend(self.incident(*id));
            }
            GraphOp::CreateEdge {
                id, source, target, ..
            } => {
                es.insert(*id);
                ns.insert(*source);
                ns.insert(*target);
            }
            GraphOp::DeleteEdge { id }
            | GraphOp::RestoreEdge { id, .. }
            | GraphOp::SetEdgeOrder { id, .. } => {
                es.insert(*id);
            }
        }
        for e in &es {
            if let Some(edge) = self.records.edges.get(e) {
                ns.insert(edge.source);
                ns.insert(edge.target);
                local.records.edges.insert(*e, edge.clone());
                local.outgoing.entry(edge.source).or_default().insert(*e);
                local.incoming.entry(edge.target).or_default().insert(*e);
            }
        }
        for n in ns {
            if let Some(life) = self.records.nodes.get(&n) {
                local.records.nodes.insert(n, life.clone());
                if life.alive() {
                    local.visible_nodes.insert(n);
                }
            }
        }
        for e in es {
            local.update_edge(e);
        }
        Ok(local.apply_changes(vec![change]))
    }
    pub fn value(&self) -> LoroValue {
        let nodes:Vec<LoroValue>=self.nodes().into_iter().map(|id|crate::fx_map!("id".to_string()=>id.to_string().into(), "meta".to_string()=>LoroValue::Container(id.associated_meta_container())).into()).collect();
        let edges:Vec<LoroValue>=self.edges().into_iter().map(|id|{let e=&self.records.edges[&id]; crate::fx_map!("id".to_string()=>id.to_string().into(),"source".to_string()=>e.source.to_string().into(),"target".to_string()=>e.target.to_string().into(),"position".to_string()=>e.order().position.to_string().into(),"meta".to_string()=>LoroValue::Container(id.associated_meta_container())).into()}).collect();
        crate::fx_map!("nodes".to_string()=>nodes.into(),"edges".to_string()=>edges.into()).into()
    }
    fn rebuild(&mut self) -> LoroResult<()> {
        let mut ids = BTreeSet::new();
        let mut validate_life = |id: ID, life: &Life| -> LoroResult<()> {
            if id.counter < 0 || !ids.insert(id) {
                return Err(invalid());
            }
            for (tag, lamport) in &life.deletes {
                if tag.counter < 0 || *lamport <= life.created_at || !ids.insert(*tag) {
                    return Err(invalid());
                }
            }
            for (restore, (lamport, tags)) in &life.restores {
                if restore.counter < 0 || *lamport <= life.created_at || !ids.insert(*restore) {
                    return Err(invalid());
                }
                let mut seen = BTreeSet::new();
                for tag in tags {
                    if !life
                        .deletes
                        .get(tag)
                        .is_some_and(|delete_lp| delete_lp < lamport)
                        || !seen.insert(tag)
                    {
                        return Err(invalid());
                    }
                }
            }
            Ok(())
        };
        for (id, n) in &self.records.nodes {
            validate_life(id.id(), n)?;
        }
        for (id, e) in &self.records.edges {
            validate_life(id.id(), &e.life)?;
        }
        for (id, edge) in &self.records.edges {
            let creation = IdLp::new(id.peer, edge.life.created_at);
            if !edge
                .orders
                .get(&creation)
                .is_some_and(|(counter, _)| *counter == id.counter)
            {
                return Err(invalid());
            }
            for (write, (counter, _)) in &edge.orders {
                if *write == creation {
                    continue;
                }
                if *counter < 0
                    || write.lamport <= creation.lamport
                    || !ids.insert(ID::new(write.peer, *counter))
                {
                    return Err(invalid());
                }
            }
        }
        for (id, life) in &mut self.records.nodes {
            if id.counter < 0 {
                return Err(invalid());
            }
            life.rebuild();
            if life.alive() {
                self.visible_nodes.insert(*id);
            }
        }
        for (id, e) in &mut self.records.edges {
            if id.counter < 0
                || !self.records.nodes.contains_key(&e.source)
                || !self.records.nodes.contains_key(&e.target)
            {
                return Err(invalid());
            }
            e.life.rebuild();
            self.outgoing.entry(e.source).or_default().insert(*id);
            self.incoming.entry(e.target).or_default().insert(*id);
        }
        for id in self.records.edges.keys().copied().collect::<Vec<_>>() {
            self.update_edge(id);
        }
        Ok(())
    }
}
impl ContainerState for GraphState {
    fn container_idx(&self) -> ContainerIdx {
        self.idx
    }
    fn is_state_empty(&self) -> bool {
        self.records.nodes.is_empty() && self.records.edges.is_empty()
    }
    fn validate_diff(&self, diff: &InternalDiff) -> LoroResult<()> {
        match diff {
            InternalDiff::Graph(d) => self.validate_changes(&d.ops),
            _ => Err(invalid()),
        }
    }
    fn apply_diff_and_convert(&mut self, diff: InternalDiff, _ctx: DiffApplyContext) -> Diff {
        let InternalDiff::Graph(d) = diff else {
            unreachable!()
        };
        Diff::Graph(self.apply_changes(d.ops))
    }
    fn apply_diff(&mut self, diff: InternalDiff, ctx: DiffApplyContext) -> LoroResult<()> {
        self.validate_diff(&diff)?;
        let _ = self.apply_diff_and_convert(diff, ctx);
        Ok(())
    }
    fn apply_local_op(&mut self, raw: &RawOp, _op: &Op) -> LoroResult<ApplyLocalOpReturn> {
        let RawOpContent::Graph(op) = &raw.content else {
            unreachable!()
        };
        let c = GraphChange {
            id: raw.id,
            lamport: raw.lamport,
            op: (**op).clone(),
            forward: true,
        };
        self.validate_changes(std::slice::from_ref(&c))?;
        self.apply_changes(vec![c]);
        Ok(Default::default())
    }
    fn to_diff(&mut self, _doc: &Weak<LoroDocInner>) -> Diff {
        let mut d = GraphDiff::default();
        for (id, l) in &self.records.nodes {
            d.ops.push(GraphChange {
                id: id.id(),
                lamport: l.created_at,
                op: GraphOp::CreateNode { id: *id },
                forward: true,
            });
            for (t, lamport) in &l.deletes {
                d.ops.push(GraphChange {
                    id: *t,
                    lamport: *lamport,
                    op: GraphOp::DeleteNode { id: *id },
                    forward: true,
                });
            }
            for (t, (lamport, ds)) in &l.restores {
                d.ops.push(GraphChange {
                    id: *t,
                    lamport: *lamport,
                    op: GraphOp::RestoreNode {
                        id: *id,
                        deletes: ds.clone(),
                    },
                    forward: true,
                });
            }
            d.nodes.insert(*id, self.node_record(*id));
        }
        for (id, e) in &self.records.edges {
            d.ops.push(GraphChange {
                id: id.id(),
                lamport: e.life.created_at,
                op: GraphOp::CreateEdge {
                    id: *id,
                    source: e.source,
                    target: e.target,
                    position: e.orders[&IdLp::new(id.peer, e.life.created_at)].1.clone(),
                },
                forward: true,
            });
            for (write, (counter, position)) in &e.orders {
                if ID::new(write.peer, *counter) == id.id() {
                    continue;
                }
                d.ops.push(GraphChange {
                    id: ID::new(write.peer, *counter),
                    lamport: write.lamport,
                    op: GraphOp::SetEdgeOrder {
                        id: *id,
                        position: position.clone(),
                    },
                    forward: true,
                });
            }
            d.orders.insert(
                *id,
                GraphOrderDelta {
                    source: e.source,
                    before: None,
                    after: Some(e.order()),
                },
            );
            for (t, lamport) in &e.life.deletes {
                d.ops.push(GraphChange {
                    id: *t,
                    lamport: *lamport,
                    op: GraphOp::DeleteEdge { id: *id },
                    forward: true,
                });
            }
            for (t, (lamport, ds)) in &e.life.restores {
                d.ops.push(GraphChange {
                    id: *t,
                    lamport: *lamport,
                    op: GraphOp::RestoreEdge {
                        id: *id,
                        deletes: ds.clone(),
                    },
                    forward: true,
                });
            }
            d.edges.insert(*id, self.edge_record(*id));
        }
        d.ops.sort_by_key(|change| (change.lamport, change.id.peer));
        Diff::Graph(d)
    }
    fn get_value(&mut self) -> LoroValue {
        self.value()
    }
    fn get_child_index(&self, id: &ContainerID) -> Option<Index> {
        if self.contains_child(id) {
            let (peer, counter, _) = id.as_normal().unwrap();
            // Like Tree metadata, Graph metadata is addressed by creation identity,
            // never by a row offset or an edge's source/target relationship.
            Some(Index::Node(TreeID {
                peer: *peer,
                counter: *counter,
            }))
        } else {
            None
        }
    }
    fn contains_child(&self, id: &ContainerID) -> bool {
        if let ContainerID::Normal {
            peer,
            counter,
            container_type: ContainerType::Map,
        } = id
        {
            self.records
                .nodes
                .contains_key(&GraphNodeId::new(*peer, *counter))
                || self
                    .records
                    .edges
                    .contains_key(&GraphEdgeId::new(*peer, *counter))
        } else {
            false
        }
    }
    fn get_child_containers(&self) -> Vec<ContainerID> {
        self.records
            .nodes
            .keys()
            .map(|id| id.associated_meta_container())
            .chain(
                self.records
                    .edges
                    .keys()
                    .map(|id| id.associated_meta_container()),
            )
            .collect()
    }
    fn fork(&self, _config: &crate::configure::Configure) -> Self {
        self.clone()
    }
}
impl FastStateSnapshot for GraphState {
    fn encode_snapshot_fast<W: std::io::Write>(&mut self, mut w: W) {
        w.write_all(&postcard::to_stdvec(&self.records).expect("graph snapshot serialization"))
            .unwrap();
    }
    fn decode_value(bytes: &[u8]) -> LoroResult<(LoroValue, &[u8])> {
        let s = Self::decode_records(
            ContainerIdx::from_index_and_type(0, ContainerType::Graph),
            bytes,
        )?;
        Ok((s.value(), bytes))
    }
    fn decode_snapshot_fast(
        idx: ContainerIdx,
        v: (LoroValue, &[u8]),
        _ctx: super::ContainerCreationContext,
    ) -> LoroResult<Self> {
        Self::decode_records(idx, v.1)
    }
}
impl GraphState {
    fn decode_records(idx: ContainerIdx, bytes: &[u8]) -> LoroResult<Self> {
        let (records, rest) = postcard::take_from_bytes(bytes).map_err(|_| invalid())?;
        if !rest.is_empty() {
            return Err(invalid());
        }
        let mut s = Self::new(idx);
        s.records = records;
        s.rebuild()?;
        Ok(s)
    }
}

#[cfg(test)]
mod ordering_tests {
    use super::*;

    fn position(first: u8) -> GraphPosition {
        GraphPosition::try_from_bytes(vec![first, 128]).unwrap()
    }

    fn change(peer: u64, counter: i32, lamport: u32, op: GraphOp) -> GraphChange {
        GraphChange {
            id: ID::new(peer, counter),
            lamport,
            op,
            forward: true,
        }
    }

    fn fixture() -> (GraphState, GraphNodeId, GraphEdgeId) {
        let mut state = GraphState::new(ContainerIdx::from_index_and_type(0, ContainerType::Graph));
        let source = GraphNodeId::new(7, 0);
        let target = GraphNodeId::new(7, 1);
        let edge = GraphEdgeId::new(7, 2);
        state.apply_changes(vec![
            change(7, 0, 0, GraphOp::CreateNode { id: source }),
            change(7, 1, 1, GraphOp::CreateNode { id: target }),
            change(
                7,
                2,
                2,
                GraphOp::CreateEdge {
                    id: edge,
                    source,
                    target,
                    position: position(128),
                },
            ),
        ]);
        (state, source, edge)
    }

    fn moved(edge: GraphEdgeId, peer: u64, counter: i32, lamport: u32, first: u8) -> GraphChange {
        change(
            peer,
            counter,
            lamport,
            GraphOp::SetEdgeOrder {
                id: edge,
                position: position(first),
            },
        )
    }

    fn inverse(mut op: GraphChange) -> GraphChange {
        op.forward = false;
        op
    }

    #[test]
    fn order_winner_uses_lamport_peer_and_retains_losing_history() {
        let (mut state, source, edge) = fixture();
        let low_clock = moved(edge, 1000, 0, 3, 240);
        let winner = moved(edge, 1, 0, 4, 16);
        state.apply_changes(vec![winner.clone(), low_clock.clone()]);
        let record = state.edge_record(edge).unwrap();
        assert_eq!(record.position, position(16));
        assert_eq!(record.last_order, IdFull::new(1, 0, 4));
        assert_eq!(state.out_edge_at(source, 0).unwrap().edge_id, edge);
        state.apply_changes(vec![inverse(winner)]);
        assert_eq!(state.edge_record(edge).unwrap().position, position(240));
        assert_eq!(state.index_of_out_edge(edge), Some(0));
        let tie_winner = moved(edge, 1001, 0, 3, 32);
        state.apply_changes(vec![tie_winner.clone()]);
        assert_eq!(state.edge_record(edge).unwrap().position, position(32));
        state.apply_changes(vec![inverse(tie_winner), inverse(low_clock)]);
        assert_eq!(state.edge_record(edge).unwrap().position, position(128));
    }

    #[test]
    fn hidden_order_survives_snapshot_and_restoration() {
        let (mut state, source, edge) = fixture();
        let deleted = change(7, 3, 3, GraphOp::DeleteNode { id: source });
        state.apply_changes(vec![deleted.clone(), moved(edge, 8, 0, 4, 16)]);
        assert!(state.ordered_out_edges(source).unwrap().is_empty());
        assert_eq!(state.index_of_out_edge(edge), None);
        state.order_jitter = 9;
        let mut bytes = Vec::new();
        state.encode_snapshot_fast(&mut bytes);
        let mut decoded = GraphState::decode_records(state.idx, &bytes).unwrap();
        assert_eq!(decoded.order_jitter, 0);
        decoded.apply_changes(vec![change(
            7,
            4,
            5,
            GraphOp::RestoreNode {
                id: source,
                deletes: vec![deleted.id],
            },
        )]);
        assert_eq!(
            decoded.out_edge_at(source, 0).unwrap().position,
            position(16)
        );
        decoded.apply_changes(vec![moved(edge, 8, 1, 6, 200)]);
        assert_eq!(decoded.index_of_out_edge(edge), Some(0));
        let mut bytes = Vec::new();
        decoded.encode_snapshot_fast(&mut bytes);
        let restored = GraphState::decode_records(decoded.idx, &bytes).unwrap();
        assert_eq!(restored.edge_record(edge), decoded.edge_record(edge));
    }

    #[test]
    fn equal_positions_use_edge_identity_and_retreat_removes_index_entry() {
        let (mut state, source, first) = fixture();
        let second = GraphEdgeId::new(9, 0);
        let create = change(
            9,
            0,
            3,
            GraphOp::CreateEdge {
                id: second,
                source,
                target: source,
                position: position(128),
            },
        );
        state.apply_changes(vec![create.clone()]);
        assert_eq!(
            state
                .ordered_out_edges(source)
                .unwrap()
                .iter()
                .map(|edge| edge.edge_id)
                .collect::<Vec<_>>(),
            vec![first, second]
        );
        assert_eq!(state.index_of_out_edge(second), Some(1));
        state.apply_changes(vec![inverse(create)]);
        assert!(state.edge_record(second).is_none());
        assert_eq!(state.ordered_out_edges(source).unwrap().len(), 1);
        assert_eq!(state.out_edge_at(source, 1), None);
    }

    #[test]
    fn full_diff_keeps_original_creation_and_all_real_write_clocks() {
        let (mut state, _, edge) = fixture();
        let write = moved(edge, 1, 0, 4, 16);
        state.apply_changes(vec![write.clone()]);
        let Diff::Graph(diff) = state.to_diff(&Weak::new()) else {
            unreachable!()
        };
        let creation = diff
            .ops
            .iter()
            .find(|change| change.id == edge.id())
            .unwrap();
        assert_eq!(creation.lamport, 2);
        assert!(
            matches!(&creation.op, GraphOp::CreateEdge { position: p, .. } if p == &position(128))
        );
        assert!(diff.ops.contains(&write));
        let mut replayed = GraphState::new(state.idx);
        replayed.validate_changes(&diff.ops).unwrap();
        replayed.apply_changes(diff.ops);
        assert_eq!(replayed.edge_record(edge), state.edge_record(edge));
        replayed.apply_changes(vec![inverse(write)]);
        assert_eq!(replayed.edge_record(edge).unwrap().position, position(128));
    }

    #[test]
    fn net_order_diff_supports_successive_undo_and_foreign_write_ids() {
        let (mut state, _, edge) = fixture();
        let first = moved(edge, 7, 3, 3, 32);
        let second = moved(edge, 7, 4, 4, 240);
        let initial = state.clone();
        let first_diff = state.apply_changes(vec![first.clone()]);
        let first_state = state.clone();
        let second_diff = state.apply_changes(vec![second.clone()]);
        let composed = first_diff.compose(second_diff);
        assert_eq!(
            composed.orders[&edge].before.as_ref().unwrap().position,
            position(128)
        );
        assert_eq!(
            composed.orders[&edge].after.as_ref().unwrap().position,
            position(240)
        );

        let mut checkout = state.clone();
        let undo_second = checkout.apply_changes(vec![inverse(second)]);
        let plan = state.local_diff_ops(&undo_second, |id| id).unwrap();
        assert_eq!(
            plan,
            vec![GraphOp::SetEdgeOrder {
                id: edge,
                position: position(32)
            }]
        );
        state.apply_changes(vec![change(7, 5, 5, plan[0].clone())]);
        let undo_first = checkout.apply_changes(vec![inverse(first)]);
        let plan = state.local_diff_ops(&undo_first, |id| id).unwrap();
        assert_eq!(
            plan,
            vec![GraphOp::SetEdgeOrder {
                id: edge,
                position: position(128)
            }]
        );

        // A copied state holds no source order writer; net inverse remains usable.
        let mut copy = initial;
        copy.apply_changes(vec![moved(edge, 99, 0, 9, 32)]);
        assert_eq!(copy.local_diff_ops(&undo_first, |id| id).unwrap(), plan);
        assert_eq!(
            first_state.edge_record(edge).unwrap().position,
            position(32)
        );
    }

    #[test]
    fn order_transform_preserves_remote_winner_but_not_unrelated_edges() {
        let (mut state, _, edge) = fixture();
        let local = moved(edge, 7, 3, 3, 32);
        state.apply_changes(vec![local.clone()]);
        let mut checkout = state.clone();
        let mut undo = checkout.apply_changes(vec![inverse(local)]);
        let remote = state.apply_changes(vec![moved(edge, 9, 0, 4, 32)]);
        // Same value, different winning identity still protects the remote edit.
        undo.transform(&remote);
        assert!(undo.orders.is_empty());
        assert!(state.local_diff_ops(&undo, |id| id).unwrap().is_empty());
    }

    #[test]
    fn split_undo_spans_keep_selected_writers_and_protect_same_peer_outside_selection() {
        let (mut state, _, edge) = fixture();
        let first = moved(edge, 7, 3, 3, 32);
        let second = moved(edge, 7, 4, 7, 240);
        state.apply_changes(vec![first.clone()]);
        let mut before = state.clone();
        let first_undo = before.apply_changes(vec![inverse(first)]);
        let between_spans = state.apply_changes(vec![second.clone()]);
        let mut undo = first_undo.clone();
        undo.transform_with_selected(&between_spans, |id| {
            loro_common::IdSpan::new(7, 3, 5).contains(id)
        });
        assert_eq!(undo.orders.len(), 1);

        // The later span is undone first when composing the final net delta.
        let second_undo = state.apply_changes(vec![inverse(second)]);
        let combined = second_undo.compose(undo);
        assert_eq!(
            combined.orders[&edge].after.as_ref().unwrap().position,
            position(128)
        );

        let outside_selection = state.apply_changes(vec![moved(edge, 7, 8, 10, 200)]);
        let mut undo = first_undo;
        undo.transform_with_selected(&outside_selection, |id| {
            loro_common::IdSpan::new(7, 3, 5).contains(id)
        });
        assert!(
            undo.orders.is_empty(),
            "same peer alone does not imply membership in this undo"
        );
    }

    #[test]
    fn snapshot_rejects_missing_creation_write_and_reused_operation_identity() {
        let (mut state, _, edge) = fixture();
        state.records.edges.get_mut(&edge).unwrap().orders.clear();
        let bytes = postcard::to_stdvec(&state.records).unwrap();
        assert!(GraphState::decode_records(state.idx, &bytes).is_err());
        let (mut state, _, edge) = fixture();
        state
            .records
            .edges
            .get_mut(&edge)
            .unwrap()
            .orders
            .insert(IdLp::new(7, 10), (1, position(16)));
        let bytes = postcard::to_stdvec(&state.records).unwrap();
        assert!(GraphState::decode_records(state.idx, &bytes).is_err());
    }
}
