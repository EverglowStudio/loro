//! Rebase a history diff into local edits without replaying obsolete operation IDs.
mod copy_order;

use super::*;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Object {
    Node(GraphNodeId),
    Edge(GraphEdgeId),
}

impl Object {
    fn of(op: &GraphOp) -> Self {
        match op {
            GraphOp::CreateNode { id }
            | GraphOp::DeleteNode { id }
            | GraphOp::RestoreNode { id, .. } => Self::Node(*id),
            GraphOp::CreateEdge { id, .. }
            | GraphOp::DeleteEdge { id }
            | GraphOp::RestoreEdge { id, .. }
            | GraphOp::SetEdgeOrder { id, .. } => Self::Edge(*id),
        }
    }

    fn delete(self) -> GraphOp {
        match self {
            Self::Node(id) => GraphOp::DeleteNode { id },
            Self::Edge(id) => GraphOp::DeleteEdge { id },
        }
    }

    fn restore(self, deletes: Vec<ID>) -> GraphOp {
        match self {
            Self::Node(id) => GraphOp::RestoreNode { id, deletes },
            Self::Edge(id) => GraphOp::RestoreEdge { id, deletes },
        }
    }
}

struct Edit {
    before: Option<Life>,
    after: Option<Life>,
    create: Option<GraphOp>,
}

impl GraphState {
    /// Apply only the touched lifecycle histories to temporary per-object copies.
    /// The resulting local edits preserve independent deletes/restores in this
    /// replica, while cancelling Delete/Restore pairs inside the supplied diff.
    /// This is an explicit diff/undo path, never part of normal CRUD or import.
    pub(crate) fn local_diff_ops(
        &self,
        diff: &GraphDiff,
        remap: impl Fn(ID) -> ID,
    ) -> LoroResult<Vec<GraphOp>> {
        let mut edits: BTreeMap<Object, Edit> = BTreeMap::new();
        for change in &diff.ops {
            change.op.validate(change.id)?;
            // Order edits use the net delta, not source operation membership.
            // A copied graph has new IDs, and an earlier Undo has a new writer.
            if matches!(change.op, GraphOp::SetEdgeOrder { .. }) {
                continue;
            }
            let object = Object::of(&change.op);
            let edit = edits.entry(object).or_insert_with(|| {
                let before = match object {
                    Object::Node(id) => self
                        .records
                        .nodes
                        .get(&GraphNodeId::from_id(remap(id.id()))),
                    Object::Edge(id) => self
                        .records
                        .edges
                        .get(&GraphEdgeId::from_id(remap(id.id())))
                        .map(|e| &e.life),
                }
                .cloned();
                Edit {
                    after: before.clone(),
                    before,
                    create: None,
                }
            });
            match &change.op {
                GraphOp::CreateNode { .. } | GraphOp::CreateEdge { .. } => {
                    if change.forward {
                        // Public apply_diff recreates objects, like other Loro
                        // containers; the handler records the metadata remap.
                        edit.before = None;
                        edit.after = Some(Life::default());
                        edit.create = Some(change.op.clone());
                    } else {
                        edit.after = None;
                    }
                }
                GraphOp::DeleteNode { .. } | GraphOp::DeleteEdge { .. } => {
                    edit.after.as_mut().ok_or_else(invalid)?.delete(
                        change.id,
                        change.lamport,
                        change.forward,
                    );
                }
                GraphOp::RestoreNode { deletes, .. } | GraphOp::RestoreEdge { deletes, .. } => {
                    let life = edit.after.as_mut().ok_or_else(invalid)?;
                    if change.forward && deletes.iter().any(|id| !life.deletes.contains_key(id)) {
                        return Err(invalid());
                    }
                    life.restore(change.id, change.lamport, deletes, change.forward);
                }
                GraphOp::SetEdgeOrder { .. } => unreachable!(),
            }
        }

        // Create nodes before edges so source/target remaps already exist.
        // Validate endpoints before writing any local operation in this Graph.
        let mut creates = Vec::new();
        for edit in edits.values() {
            if edit.after.is_none() {
                continue;
            }
            if let Some(op) = &edit.create {
                if let GraphOp::CreateEdge { source, target, .. } = op {
                    for id in [source, target] {
                        let exists = self
                            .records
                            .nodes
                            .contains_key(&GraphNodeId::from_id(remap(id.id())))
                            || edits
                                .get(&Object::Node(*id))
                                .is_some_and(|e| e.after.is_some());
                        if !exists {
                            return Err(invalid());
                        }
                    }
                }
                let mut op = op.clone();
                if let GraphOp::CreateEdge { id, position, .. } = &mut op {
                    if let Some(after) = diff.orders.get(id).and_then(|delta| delta.after.as_ref())
                    {
                        *position = after.position.clone();
                    }
                }
                creates.push(op);
            }
        }
        // Key by the destination's existing identity so copy-order adjustments
        // replace a planned write, including when the diff uses a remapped ID.
        let mut orders = BTreeMap::new();
        for (id, delta) in &diff.orders {
            let Some(after) = &delta.after else {
                continue;
            };
            if delta
                .before
                .as_ref()
                .is_some_and(|before| before.position == after.position)
            {
                continue;
            }
            // Creation already carries the final position. Retreating creation
            // becomes a lifecycle edit, never a position write to an absent edge.
            if edits
                .get(&Object::Edge(*id))
                .is_some_and(|edit| edit.create.is_some() || edit.after.is_none())
            {
                continue;
            }
            let current_id = GraphEdgeId::from_id(remap(id.id()));
            let current = self.records.edges.get(&current_id).ok_or_else(invalid)?;
            if current.source != GraphNodeId::from_id(remap(delta.source.id())) {
                return Err(invalid());
            }
            if current.order().position != after.position {
                orders.insert(current_id, (*id, after.position.clone()));
            }
        }
        // All key allocation and validation completes before this method returns
        // any local write. Ordinary create/reorder and remote replay bypass it.
        self.plan_copied_edge_order(&edits, &remap, &mut creates, &mut orders)?;
        let mut result = creates;
        for (object, edit) in edits {
            let Some(after) = edit.after else {
                if edit.before.as_ref().is_some_and(Life::alive) {
                    result.push(object.delete());
                }
                continue;
            };
            if edit.create.is_some() {
                if !after.alive() {
                    result.push(object.delete());
                }
                continue;
            }
            let before: BTreeSet<_> = edit
                .before
                .ok_or_else(invalid)?
                .active()
                .into_iter()
                .collect();
            let after: BTreeSet<_> = after.active().into_iter().collect();
            let removed: Vec<_> = before.difference(&after).copied().collect();
            if !removed.is_empty() {
                result.push(object.restore(removed));
            }
            if after.difference(&before).next().is_some() {
                // Reintroducing a historical delete or adding a new one needs
                // one fresh delete, not a forged reuse of the old operation ID.
                result.push(object.delete());
            }
        }
        result.extend(orders.into_iter().filter_map(|(current, (id, position))| {
            (self.records.edges[&current].order().position != position)
                .then_some(GraphOp::SetEdgeOrder { id, position })
        }));
        Ok(result)
    }
}
