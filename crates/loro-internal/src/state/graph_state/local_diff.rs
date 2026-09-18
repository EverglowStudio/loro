//! Rebase a history diff into local edits without replaying obsolete operation IDs.
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
            | GraphOp::RestoreEdge { id, .. } => Self::Edge(*id),
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
        changes: &[GraphChange],
        remap: impl Fn(ID) -> ID,
    ) -> LoroResult<Vec<GraphOp>> {
        let mut edits: BTreeMap<Object, Edit> = BTreeMap::new();
        for change in changes {
            change.op.validate(change.id)?;
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
                    edit.after
                        .as_mut()
                        .ok_or_else(invalid)?
                        .delete(change.id, change.forward);
                }
                GraphOp::RestoreNode { deletes, .. } | GraphOp::RestoreEdge { deletes, .. } => {
                    let life = edit.after.as_mut().ok_or_else(invalid)?;
                    if change.forward && deletes.iter().any(|id| !life.deletes.contains(id)) {
                        return Err(invalid());
                    }
                    life.restore(change.id, deletes, change.forward);
                }
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
                creates.push(op.clone());
            }
        }
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
        Ok(result)
    }
}
