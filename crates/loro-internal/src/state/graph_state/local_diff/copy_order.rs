//! Preserve editable-copy order when fresh edge identities change a key's tie-break.
use super::*;

#[derive(Clone, Copy)]
enum Destination {
    Create(usize),
    Existing(GraphEdgeId),
}

struct CopyOrderEdge {
    id: GraphEdgeId,
    position: GraphPosition,
    destination: Destination,
}

impl GraphState {
    pub(super) fn plan_copied_edge_order(
        &self,
        edits: &BTreeMap<Object, Edit>,
        remap: &impl Fn(ID) -> ID,
        creates: &mut [GraphOp],
        orders: &mut BTreeMap<GraphEdgeId, (GraphEdgeId, GraphPosition)>,
    ) -> LoroResult<()> {
        if !creates
            .iter()
            .any(|op| matches!(op, GraphOp::CreateEdge { .. }))
        {
            return Ok(());
        }

        // A copied object is distinct from an existing object it may have mapped
        // to earlier. Only non-creation edits change those existing lifecycles.
        let mut node_alive = BTreeMap::new();
        let mut edge_alive = BTreeMap::new();
        for (object, edit) in edits {
            if edit.create.is_some() {
                continue;
            }
            let alive = edit.after.as_ref().is_some_and(Life::alive);
            match object {
                Object::Node(id) => {
                    node_alive.insert(GraphNodeId::from_id(remap(id.id())), alive);
                }
                Object::Edge(id) => {
                    edge_alive.insert(GraphEdgeId::from_id(remap(id.id())), alive);
                }
            }
        }
        let existing_node_alive = |id: GraphNodeId| {
            node_alive
                .get(&id)
                .copied()
                .unwrap_or_else(|| self.records.nodes.get(&id).is_some_and(Life::alive))
        };
        let copied_endpoint_alive = |id: GraphNodeId| {
            if let Some(edit) = edits
                .get(&Object::Node(id))
                .filter(|edit| edit.create.is_some())
            {
                edit.after.as_ref().is_some_and(Life::alive)
            } else {
                existing_node_alive(GraphNodeId::from_id(remap(id.id())))
            }
        };

        let mut sources: BTreeMap<GraphNodeId, Vec<CopyOrderEdge>> = BTreeMap::new();
        for (index, op) in creates.iter().enumerate() {
            let GraphOp::CreateEdge {
                id,
                source,
                target,
                position,
            } = op
            else {
                continue;
            };
            if !edits
                .get(&Object::Edge(*id))
                .is_some_and(|edit| edit.after.as_ref().is_some_and(Life::alive))
                || !copied_endpoint_alive(*source)
                || !copied_endpoint_alive(*target)
            {
                continue;
            }
            // A fresh source has only copied edges. The create plan already emits
            // them in original EdgeId order, so its equal-key ties remain intact.
            if edits
                .get(&Object::Node(*source))
                .is_some_and(|edit| edit.create.is_some())
            {
                continue;
            }
            sources
                .entry(GraphNodeId::from_id(remap(source.id())))
                .or_default()
                .push(CopyOrderEdge {
                    id: *id,
                    position: position.clone(),
                    destination: Destination::Create(index),
                });
        }

        let mut assignments = Vec::new();
        for (source, mut sequence) in sources {
            // Include restored edges, exclude net-hidden edges, and use planned
            // positions before sorting. Current visibility alone is insufficient.
            for id in self.outgoing.get(&source).into_iter().flatten() {
                let edge = &self.records.edges[id];
                if !edge_alive
                    .get(id)
                    .copied()
                    .unwrap_or_else(|| edge.life.alive())
                    || !existing_node_alive(edge.source)
                    || !existing_node_alive(edge.target)
                {
                    continue;
                }
                let position = orders
                    .get(id)
                    .map(|(_, position)| position.clone())
                    .unwrap_or_else(|| edge.order().position);
                sequence.push(CopyOrderEdge {
                    id: *id,
                    position,
                    destination: Destination::Existing(*id),
                });
            }
            sequence.sort_by(|a, b| (&a.position, a.id).cmp(&(&b.position, b.id)));
            assignments.extend(
                assign_mixed_groups(&sequence, self.order_jitter)
                    .map_err(|error| LoroError::ArgErr(error.to_string().into()))?,
            );
        }

        // Allocation can fail (for example at the key length limit). Do not even
        // mutate the caller's local plan until every affected source succeeded.
        for (destination, position) in assignments {
            match destination {
                Destination::Create(index) => {
                    let GraphOp::CreateEdge {
                        position: initial, ..
                    } = &mut creates[index]
                    else {
                        unreachable!()
                    };
                    *initial = position;
                }
                Destination::Existing(id) => {
                    orders
                        .entry(id)
                        .and_modify(|(_, planned)| *planned = position.clone())
                        .or_insert((id, position));
                }
            }
        }
        Ok(())
    }
}

fn assign_mixed_groups(
    sequence: &[CopyOrderEdge],
    jitter: u8,
) -> Result<Vec<(Destination, GraphPosition)>, crate::container::graph::GraphOrderError> {
    let mut assignments = Vec::new();
    let mut start = 0;
    let mut left = None;
    while start < sequence.len() {
        let position = &sequence[start].position;
        let end = start + sequence[start..].partition_point(|edge| &edge.position == position);
        let group = &sequence[start..end];
        let first_existing = group
            .iter()
            .position(|edge| matches!(edge.destination, Destination::Existing(_)));
        if let Some(first_existing) = first_existing {
            // A leading copied prefix can move before the existing group without
            // changing any existing edge's register.
            let prefix = &group[..first_existing];
            let keys = GraphPosition::evenly(left.as_ref(), Some(position), prefix.len(), jitter)?;
            assignments.extend(
                prefix
                    .iter()
                    .zip(keys)
                    .map(|(edge, key)| (edge.destination, key)),
            );

            let suffix_start = group[first_existing..]
                .iter()
                .position(|edge| matches!(edge.destination, Destination::Create(_)))
                .map(|offset| first_existing + offset);
            if let Some(suffix_start) = suffix_start {
                // Keep the existing prefix at its key; only the necessary mixed
                // suffix is moved into the next strict gap as ordinary writes.
                let suffix = &group[suffix_start..];
                let right = sequence.get(end).map(|edge| &edge.position);
                let keys = GraphPosition::evenly(Some(position), right, suffix.len(), jitter)?;
                left = keys.last().cloned();
                assignments.extend(
                    suffix
                        .iter()
                        .zip(keys)
                        .map(|(edge, key)| (edge.destination, key)),
                );
            } else {
                left = Some(position.clone());
            }
        } else {
            left = Some(position.clone());
        }
        // The next logical group is found using the unchanged original keys.
        // Its copied prefix must follow this group's *assigned* last key, or
        // neighboring mixed groups could allocate overlapping/interleaved keys.
        start = end;
    }
    Ok(assignments)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(byte: u8) -> GraphPosition {
        GraphPosition::try_from_bytes(vec![byte, 128]).unwrap()
    }

    fn apply(state: &mut GraphState, id: ID, lamport: u32, op: GraphOp) -> GraphDiff {
        state.apply_changes(vec![GraphChange {
            id,
            lamport,
            op,
            forward: true,
        }])
    }

    fn seed() -> (GraphState, GraphNodeId) {
        let mut state = GraphState::new(ContainerIdx::from_index_and_type(0, ContainerType::Graph));
        let node = GraphNodeId::new(99, 0);
        apply(&mut state, node.id(), 0, GraphOp::CreateNode { id: node });
        (state, node)
    }

    fn create(id: GraphEdgeId, source: GraphNodeId, position: GraphPosition) -> GraphOp {
        GraphOp::CreateEdge {
            id,
            source,
            target: source,
            position,
        }
    }

    #[test]
    fn mixed_group_order_is_independent_of_the_future_copy_peer() {
        // Every arrangement of copied/existing edges in a six-way key collision.
        for mask in 0..64 {
            let mut copy_index = 0;
            let sequence: Vec<_> = (0..6)
                .map(|i| {
                    let id = GraphEdgeId::new(5, i);
                    let destination = if mask & (1 << i) != 0 {
                        let index = copy_index;
                        copy_index += 1;
                        Destination::Create(index)
                    } else {
                        Destination::Existing(id)
                    };
                    CopyOrderEdge {
                        id,
                        position: key(128),
                        destination,
                    }
                })
                .collect();
            let assignments = assign_mixed_groups(&sequence, 0).unwrap();
            let new_keys: BTreeMap<_, _> = assignments.iter().map(|(destination, key)| {
                let index = match destination {
                    Destination::Create(index) => sequence.iter().position(|edge| matches!(edge.destination, Destination::Create(i) if i == *index)).unwrap(),
                    Destination::Existing(id) => sequence.iter().position(|edge| edge.id == *id).unwrap(),
                };
                (index, key.clone())
            }).collect();
            for peer in [1, 5, 20] {
                let mut actual: Vec<_> = sequence
                    .iter()
                    .enumerate()
                    .map(|(index, edge)| {
                        let id = match edge.destination {
                            Destination::Create(copy) => GraphEdgeId::new(peer, 100 + copy as i32),
                            Destination::Existing(id) => id,
                        };
                        (
                            new_keys.get(&index).unwrap_or(&edge.position).clone(),
                            id,
                            index,
                        )
                    })
                    .collect();
                actual.sort();
                assert_eq!(
                    actual
                        .iter()
                        .map(|(_, _, index)| *index)
                        .collect::<Vec<_>>(),
                    (0..6).collect::<Vec<_>>(),
                    "mask {mask}, peer {peer}"
                );
            }
        }
    }

    #[test]
    fn neighboring_mixed_groups_do_not_overlap_their_allocated_gap() {
        let sequence = [
            CopyOrderEdge {
                id: GraphEdgeId::new(1, 0),
                position: key(64),
                destination: Destination::Existing(GraphEdgeId::new(1, 0)),
            },
            CopyOrderEdge {
                id: GraphEdgeId::new(20, 0),
                position: key(64),
                destination: Destination::Create(1),
            },
            CopyOrderEdge {
                id: GraphEdgeId::new(30, 0),
                position: key(64),
                destination: Destination::Existing(GraphEdgeId::new(30, 0)),
            },
            CopyOrderEdge {
                id: GraphEdgeId::new(2, 0),
                position: key(128),
                destination: Destination::Create(0),
            },
            CopyOrderEdge {
                id: GraphEdgeId::new(9, 0),
                position: key(128),
                destination: Destination::Existing(GraphEdgeId::new(9, 0)),
            },
        ];
        let assignments = assign_mixed_groups(&sequence, 0).unwrap();
        let mut keys: Vec<_> = sequence.iter().map(|edge| edge.position.clone()).collect();
        for (destination, position) in assignments {
            let index = match destination {
                Destination::Create(1) => 1,
                Destination::Existing(id) if id == GraphEdgeId::new(30, 0) => 2,
                Destination::Create(0) => 3,
                _ => panic!("unnecessary assignment"),
            };
            keys[index] = position;
        }
        assert!(keys.windows(2).all(|pair| pair[0] < pair[1]));
    }

    #[test]
    fn planned_order_is_used_and_auxiliary_write_replaces_it() {
        let (mut state, node) = seed();
        let before = GraphEdgeId::new(1, 0);
        let copied = GraphEdgeId::new(2, 0);
        let after = GraphEdgeId::new(9, 0);
        apply(&mut state, before.id(), 1, create(before, node, key(128)));
        apply(&mut state, after.id(), 1, create(after, node, key(240)));
        let mut source = state.clone();
        let diff =
            apply(&mut source, copied.id(), 1, create(copied, node, key(128))).compose(apply(
                &mut source,
                ID::new(9, 1),
                2,
                GraphOp::SetEdgeOrder {
                    id: after,
                    position: key(128),
                },
            ));
        let plan = state.local_diff_ops(&diff, |id| id).unwrap();
        let copied_position = plan
            .iter()
            .find_map(|op| match op {
                GraphOp::CreateEdge { id, position, .. } if *id == copied => Some(position),
                _ => None,
            })
            .unwrap();
        let order_writes: Vec<_> = plan
            .iter()
            .filter_map(|op| match op {
                GraphOp::SetEdgeOrder { id, position } => Some((*id, position)),
                _ => None,
            })
            .collect();
        assert_eq!(order_writes.len(), 1);
        assert_eq!(order_writes[0].0, after);
        assert!(key(128) < *copied_position && copied_position < order_writes[0].1);
        assert_eq!(state.edge_record(after).unwrap().position, key(240));
    }

    #[test]
    fn net_lifecycle_hides_deleted_edges_and_keeps_independent_remote_delete() {
        let (mut state, node) = seed();
        let existing = GraphEdgeId::new(9, 0);
        let copied = GraphEdgeId::new(2, 0);
        apply(
            &mut state,
            existing.id(),
            1,
            create(existing, node, key(128)),
        );
        let mut source = state.clone();
        let diff =
            apply(&mut source, copied.id(), 1, create(copied, node, key(128))).compose(apply(
                &mut source,
                ID::new(9, 1),
                2,
                GraphOp::DeleteEdge { id: existing },
            ));
        let plan = state.local_diff_ops(&diff, |id| id).unwrap();
        assert!(plan
            .iter()
            .any(|op| matches!(op, GraphOp::CreateEdge { position, .. } if *position == key(128))));
        assert!(!plan
            .iter()
            .any(|op| matches!(op, GraphOp::SetEdgeOrder { .. })));

        // A restore of one observed delete must not unhide an independently
        // deleted source, or allocate copy order against its hidden adjacency.
        apply(
            &mut state,
            ID::new(9, 1),
            2,
            GraphOp::DeleteNode { id: node },
        );
        let mut source = state.clone();
        let diff =
            apply(&mut source, copied.id(), 3, create(copied, node, key(128))).compose(apply(
                &mut source,
                ID::new(9, 2),
                4,
                GraphOp::RestoreNode {
                    id: node,
                    deletes: vec![ID::new(9, 1)],
                },
            ));
        apply(
            &mut state,
            ID::new(20, 0),
            3,
            GraphOp::DeleteNode { id: node },
        );
        let plan = state.local_diff_ops(&diff, |id| id).unwrap();
        assert!(plan
            .iter()
            .any(|op| matches!(op, GraphOp::CreateEdge { position, .. } if *position == key(128))));
        assert!(!plan
            .iter()
            .any(|op| matches!(op, GraphOp::SetEdgeOrder { .. })));
        assert!(!state.node_record(node).unwrap().alive);
    }

    #[test]
    fn failed_copy_key_allocation_does_not_change_state() {
        let (mut state, node) = seed();
        let existing = GraphEdgeId::new(9, 0);
        let copied = GraphEdgeId::new(2, 0);
        let mut bytes = vec![0; 4095];
        bytes.push(128);
        let minimum = GraphPosition::try_from_bytes(bytes).unwrap();
        apply(
            &mut state,
            existing.id(),
            1,
            create(existing, node, minimum.clone()),
        );
        let mut source = state.clone();
        let diff = apply(&mut source, copied.id(), 1, create(copied, node, minimum));
        let before = state.value();
        let writer = state.edge_record(existing).unwrap().last_order;
        assert!(state.local_diff_ops(&diff, |id| id).is_err());
        assert_eq!(state.value(), before);
        assert_eq!(state.edge_record(existing).unwrap().last_order, writer);
        assert!(state.edge_record(copied).is_none());
    }
}
