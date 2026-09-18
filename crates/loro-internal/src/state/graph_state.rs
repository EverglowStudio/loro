//! Incremental remove-wins graph state. Only incident edges are re-evaluated on node changes.
mod local_diff;

use super::{ApplyLocalOpReturn, ContainerState, DiffApplyContext, FastStateSnapshot};
use crate::{
    container::{
        graph::{GraphChange, GraphDiff, GraphEdge, GraphNode, GraphOp},
        idx::ContainerIdx,
    },
    event::{Diff, Index, InternalDiff},
    op::{Op, RawOp, RawOpContent},
    LoroDocInner,
};
use loro_common::{
    ContainerID, ContainerType, GraphEdgeId, GraphNodeId, LoroError, LoroResult, LoroValue, TreeID,
    ID,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Weak,
};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Life {
    deletes: BTreeSet<ID>,
    // Retain restore identities so history retreat can remove exactly one restore.
    restores: BTreeMap<ID, Vec<ID>>,
    #[serde(skip)]
    removed: BTreeMap<ID, usize>,
}
impl Life {
    fn active(&self) -> Vec<ID> {
        self.deletes
            .iter()
            .filter(|id| !self.removed.contains_key(id))
            .copied()
            .collect()
    }
    fn alive(&self) -> bool {
        self.deletes.iter().all(|id| self.removed.contains_key(id))
    }
    fn delete(&mut self, id: ID, forward: bool) {
        if forward {
            self.deletes.insert(id);
        } else {
            self.deletes.remove(&id);
        }
    }
    fn restore(&mut self, id: ID, deletes: &[ID], forward: bool) {
        if forward {
            if self.restores.insert(id, deletes.to_vec()).is_none() {
                for tag in deletes {
                    *self.removed.entry(*tag).or_default() += 1;
                }
            }
        } else if let Some(tags) = self.restores.remove(&id) {
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
        for tags in self.restores.values() {
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
        Some(GraphEdge {
            id,
            source: edge.source,
            target: edge.target,
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
        let visible = self.records.edges.get(&id).is_some_and(|e| {
            e.life.alive()
                && self.visible_nodes.contains(&e.source)
                && self.visible_nodes.contains(&e.target)
        });
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
                GraphOp::CreateEdge { id, source, target } => {
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
                GraphOp::DeleteEdge { id } | GraphOp::RestoreEdge { id, .. } => {
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
                                .is_some_and(|n| n.deletes.contains(tag))
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
                                .is_some_and(|e| e.life.deletes.contains(tag))
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
            match &c.op {
                GraphOp::CreateNode { id } => {
                    ns.insert(*id);
                    if c.forward {
                        self.records.nodes.entry(*id).or_default();
                    } else {
                        self.records.nodes.remove(id);
                    }
                }
                GraphOp::CreateEdge { id, source, target } => {
                    es.insert(*id);
                    if c.forward {
                        self.records.edges.insert(
                            *id,
                            Edge {
                                source: *source,
                                target: *target,
                                life: Life::default(),
                            },
                        );
                        self.outgoing.entry(*source).or_default().insert(*id);
                        self.incoming.entry(*target).or_default().insert(*id);
                    } else {
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
                        .delete(c.id, c.forward);
                }
                GraphOp::DeleteEdge { id } => {
                    es.insert(*id);
                    self.records
                        .edges
                        .get_mut(id)
                        .expect("validated edge")
                        .life
                        .delete(c.id, c.forward);
                }
                GraphOp::RestoreNode { id, deletes } => {
                    ns.insert(*id);
                    self.records
                        .nodes
                        .get_mut(id)
                        .expect("validated node")
                        .restore(c.id, deletes, c.forward);
                }
                GraphOp::RestoreEdge { id, deletes } => {
                    es.insert(*id);
                    self.records
                        .edges
                        .get_mut(id)
                        .expect("validated edge")
                        .life
                        .restore(c.id, deletes, c.forward);
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
            GraphOp::CreateEdge { id, source, target } => {
                es.insert(*id);
                ns.insert(*source);
                ns.insert(*target);
            }
            GraphOp::DeleteEdge { id } | GraphOp::RestoreEdge { id, .. } => {
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
        let edges:Vec<LoroValue>=self.edges().into_iter().map(|id|{let e=&self.records.edges[&id]; crate::fx_map!("id".to_string()=>id.to_string().into(),"source".to_string()=>e.source.to_string().into(),"target".to_string()=>e.target.to_string().into(),"meta".to_string()=>LoroValue::Container(id.associated_meta_container())).into()}).collect();
        crate::fx_map!("nodes".to_string()=>nodes.into(),"edges".to_string()=>edges.into()).into()
    }
    fn rebuild(&mut self) -> LoroResult<()> {
        let mut ids = BTreeSet::new();
        let mut validate_life = |id: ID, life: &Life| -> LoroResult<()> {
            if id.counter < 0 || !ids.insert(id) {
                return Err(invalid());
            }
            for tag in &life.deletes {
                if tag.counter < 0 || !ids.insert(*tag) {
                    return Err(invalid());
                }
            }
            for (restore, tags) in &life.restores {
                if restore.counter < 0 || !ids.insert(*restore) {
                    return Err(invalid());
                }
                let mut seen = BTreeSet::new();
                for tag in tags {
                    if !life.deletes.contains(tag) || !seen.insert(tag) {
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
                op: GraphOp::CreateNode { id: *id },
                forward: true,
            });
            for t in &l.deletes {
                d.ops.push(GraphChange {
                    id: *t,
                    op: GraphOp::DeleteNode { id: *id },
                    forward: true,
                });
            }
            for (t, ds) in &l.restores {
                d.ops.push(GraphChange {
                    id: *t,
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
                op: GraphOp::CreateEdge {
                    id: *id,
                    source: e.source,
                    target: e.target,
                },
                forward: true,
            });
            for t in &e.life.deletes {
                d.ops.push(GraphChange {
                    id: *t,
                    op: GraphOp::DeleteEdge { id: *id },
                    forward: true,
                });
            }
            for (t, ds) in &e.life.restores {
                d.ops.push(GraphChange {
                    id: *t,
                    op: GraphOp::RestoreEdge {
                        id: *id,
                        deletes: ds.clone(),
                    },
                    forward: true,
                });
            }
            d.edges.insert(*id, self.edge_record(*id));
        }
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
