//! Opt-in analysis and repair of a visible graph snapshot.
//!
//! The graph CRDT itself accepts cycles. These helpers never participate in merge.
//! Analysis is iterative, deterministic, and does not create CRDT operations.
use std::collections::{BTreeMap, BTreeSet, VecDeque};

use loro_common::{ContainerID, GraphEdgeId, GraphNodeId, LoroError};
use thiserror::Error;

use crate::{handler::GraphHandler, version::Frontiers, HandlerTrait};

/// An immutable edge in a visible graph snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphSnapshotEdge {
    pub id: GraphEdgeId,
    pub source: GraphNodeId,
    pub target: GraphNodeId,
}

/// A committed DocState snapshot, including its container identity.
///
/// Capture fails when local edits have not been committed. Capturing a snapshot
/// never commits those edits implicitly. A detached historical DocState may be
/// inspected; applying a plan still requires that document to be editable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphSnapshot {
    graph: ContainerID,
    version: Frontiers,
    nodes: Vec<GraphNodeId>,
    edges: Vec<GraphSnapshotEdge>,
}

impl GraphSnapshot {
    pub(crate) fn new(
        graph: ContainerID,
        version: Frontiers,
        mut nodes: Vec<GraphNodeId>,
        mut edges: Vec<GraphSnapshotEdge>,
    ) -> Self {
        nodes.sort_unstable();
        edges.sort_unstable_by_key(|edge| edge.id);
        Self {
            graph,
            version,
            nodes,
            edges,
        }
    }

    pub fn graph(&self) -> &ContainerID {
        &self.graph
    }
    pub fn version(&self) -> &Frontiers {
        &self.version
    }
    pub fn nodes(&self) -> &[GraphNodeId] {
        &self.nodes
    }
    pub fn edges(&self) -> &[GraphSnapshotEdge] {
        &self.edges
    }
}

/// Analysis and repair failures have no implicit retry or enlarged scope.
#[derive(Debug, Error, PartialEq)]
pub enum GraphRepairError {
    #[error(transparent)]
    Loro(#[from] LoroError),
    #[error("Commit pending local edits explicitly before graph analysis or repair")]
    UncommittedChanges,
    #[error("The graph repair plan was made for a different DocState version")]
    StalePlan,
    #[error("The graph repair plan belongs to another graph container")]
    WrongGraph,
    #[error("Graph snapshots require an attached container")]
    DetachedContainer,
    #[error("Selected edge {0} is not visible in the snapshot")]
    InvalidSelection(GraphEdgeId),
    #[error("Invalid graph repair plan")]
    InvalidPlan,
}

/// Explicitly selected edges are canonicalized and must all be visible.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GraphScope {
    AllVisible,
    Selected(Vec<GraphEdgeId>),
}

/// One cyclic strongly connected component and a directed cycle witness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CyclicComponent {
    /// Stable ID order; a singleton is cyclic only if it has a self-loop.
    pub nodes: Vec<GraphNodeId>,
    /// A nonempty sequence of edge IDs forming one directed cycle.
    pub witness: Vec<GraphEdgeId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CycleReport {
    /// All SCCs, including isolated nodes, ordered by the smallest node ID.
    pub components: Vec<Vec<GraphNodeId>>,
    pub cyclic_components: Vec<CyclicComponent>,
    pub self_loops: Vec<GraphEdgeId>,
    pub scope: GraphScope,
}

impl CycleReport {
    pub fn is_acyclic(&self) -> bool {
        self.cyclic_components.is_empty()
    }
}

/// Repair policies are local planning choices, never replicated commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepairPolicy {
    /// In each cyclic SCC, keep only edges with source ID < target ID.
    /// Cross-component edges and edges outside the scope are untouched.
    /// This is linear after sorting and is not a minimum feedback edge set.
    AscendingNodeIdV1,
}

impl RepairPolicy {
    pub fn id(self) -> &'static str {
        "ascending-node-id"
    }
    pub fn version(self) -> u32 {
        1
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepairReason {
    SelfLoop,
    /// Index in the stable SCC list returned by `analyze_cycles`.
    DescendingEdgeInComponent {
        component: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepairDeletion {
    pub edge: GraphEdgeId,
    pub reason: RepairReason,
}

/// An immutable plan produced by `plan_break_cycles`.
///
/// Applying it generates only concrete normal edge deletions. New edges and
/// restored nodes/edges may create cycles again. Concurrent plans can converge
/// while deleting more edges than a single plan would have deleted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepairPlan {
    graph: ContainerID,
    version: Frontiers,
    policy: RepairPolicy,
    scope: GraphScope,
    deletions: Vec<RepairDeletion>,
}

impl RepairPlan {
    pub fn graph(&self) -> &ContainerID {
        &self.graph
    }
    pub fn version(&self) -> &Frontiers {
        &self.version
    }
    pub fn policy(&self) -> RepairPolicy {
        self.policy
    }
    pub fn scope(&self) -> &GraphScope {
        &self.scope
    }
    pub fn deletions(&self) -> &[RepairDeletion] {
        &self.deletions
    }
    pub fn is_empty(&self) -> bool {
        self.deletions.is_empty()
    }
}

struct IndexedGraph<'a> {
    edges: Vec<&'a GraphSnapshotEdge>,
    scope: GraphScope,
    outgoing: Vec<Vec<(usize, GraphEdgeId)>>,
    incoming: Vec<Vec<(usize, GraphEdgeId)>>,
    positions: BTreeMap<GraphNodeId, usize>,
}

impl<'a> IndexedGraph<'a> {
    fn new(snapshot: &'a GraphSnapshot, scope: &GraphScope) -> Result<Self, GraphRepairError> {
        let positions: BTreeMap<_, _> = snapshot
            .nodes
            .iter()
            .copied()
            .enumerate()
            .map(|(index, node)| (node, index))
            .collect();
        let (edges, scope) = match scope {
            GraphScope::AllVisible => (
                snapshot.edges.iter().collect::<Vec<_>>(),
                GraphScope::AllVisible,
            ),
            GraphScope::Selected(ids) => {
                let selected: BTreeSet<_> = ids.iter().copied().collect();
                let by_id: BTreeMap<_, _> =
                    snapshot.edges.iter().map(|edge| (edge.id, edge)).collect();
                let mut edges = Vec::with_capacity(selected.len());
                for id in &selected {
                    edges.push(
                        *by_id
                            .get(id)
                            .ok_or(GraphRepairError::InvalidSelection(*id))?,
                    );
                }
                (edges, GraphScope::Selected(selected.into_iter().collect()))
            }
        };
        let mut outgoing = vec![Vec::new(); snapshot.nodes.len()];
        let mut incoming = outgoing.clone();
        for edge in &edges {
            let &source = positions
                .get(&edge.source)
                .ok_or(GraphRepairError::InvalidPlan)?;
            let &target = positions
                .get(&edge.target)
                .ok_or(GraphRepairError::InvalidPlan)?;
            outgoing[source].push((target, edge.id));
            incoming[target].push((source, edge.id));
        }
        // Input edge order is stable, so witnesses are independent of hash order.
        Ok(Self {
            edges,
            scope,
            outgoing,
            incoming,
            positions,
        })
    }

    fn components(&self) -> (Vec<Vec<usize>>, Vec<usize>) {
        let n = self.outgoing.len();
        let mut visited = vec![false; n];
        let mut finish = Vec::with_capacity(n);
        for start in 0..n {
            if visited[start] {
                continue;
            }
            visited[start] = true;
            let mut stack = vec![(start, 0usize)];
            while let Some((node, next)) = stack.last_mut() {
                if *next == self.outgoing[*node].len() {
                    finish.push(*node);
                    stack.pop();
                } else {
                    let child = self.outgoing[*node][*next].0;
                    *next += 1;
                    if !visited[child] {
                        visited[child] = true;
                        stack.push((child, 0));
                    }
                }
            }
        }
        visited.fill(false);
        let mut components = Vec::new();
        for start in finish.into_iter().rev() {
            if visited[start] {
                continue;
            }
            visited[start] = true;
            let mut stack = vec![start];
            let mut component = Vec::new();
            while let Some(node) = stack.pop() {
                component.push(node);
                for &(parent, _) in &self.incoming[node] {
                    if !visited[parent] {
                        visited[parent] = true;
                        stack.push(parent);
                    }
                }
            }
            component.sort_unstable();
            components.push(component);
        }
        components.sort_unstable_by_key(|component| component[0]);
        let mut membership = vec![0; n];
        for (index, component) in components.iter().enumerate() {
            for &node in component {
                membership[node] = index;
            }
        }
        (components, membership)
    }

    fn witness(&self, component: usize, nodes: &[usize], membership: &[usize]) -> Vec<GraphEdgeId> {
        let start = nodes[0];
        let Some(&(target, first)) = self.outgoing[start]
            .iter()
            .find(|&&(target, _)| membership[target] == component)
        else {
            return Vec::new();
        };
        if target == start {
            return vec![first];
        }
        // A path back exists inside an SCC. Allocate only the component's visited
        // set so a graph of many tiny SCCs does not become quadratic.
        let mut predecessor = BTreeMap::new();
        predecessor.insert(target, None);
        let mut queue = VecDeque::from([target]);
        while let Some(node) = queue.pop_front() {
            for &(next, edge) in &self.outgoing[node] {
                if membership[next] != component || predecessor.contains_key(&next) {
                    continue;
                }
                predecessor.insert(next, Some((node, edge)));
                if next == start {
                    let mut path = Vec::new();
                    let mut cursor = start;
                    while let Some(&(parent, id)) =
                        predecessor.get(&cursor).and_then(Option::as_ref)
                    {
                        path.push(id);
                        cursor = parent;
                    }
                    path.reverse();
                    path.insert(0, first);
                    return path;
                }
                queue.push_back(next);
            }
        }
        Vec::new()
    }
}

/// Iterative SCC analysis with one stable witness per cyclic component.
///
/// ID indexing and canonicalization cost O((V + E) log(V + E)); the SCC walks
/// use O(V + E) time and memory. Witness search never enumerates simple cycles.
pub fn analyze_cycles(
    snapshot: &GraphSnapshot,
    scope: &GraphScope,
) -> Result<CycleReport, GraphRepairError> {
    let graph = IndexedGraph::new(snapshot, scope)?;
    let (components, membership) = graph.components();
    let self_loops = graph
        .edges
        .iter()
        .filter(|edge| edge.source == edge.target)
        .map(|edge| edge.id)
        .collect();
    let mut cyclic_components = Vec::new();
    for (component, nodes) in components.iter().enumerate() {
        let cyclic = nodes.len() > 1
            || graph.outgoing[nodes[0]]
                .iter()
                .any(|&(target, _)| target == nodes[0]);
        if cyclic {
            cyclic_components.push(CyclicComponent {
                nodes: nodes.iter().map(|&node| snapshot.nodes[node]).collect(),
                witness: graph.witness(component, nodes, &membership),
            });
        }
    }
    Ok(CycleReport {
        components: components
            .iter()
            .map(|nodes| nodes.iter().map(|&node| snapshot.nodes[node]).collect())
            .collect(),
        cyclic_components,
        self_loops,
        scope: graph.scope,
    })
}

/// Produce a deterministic plan without changing counters, frontiers or events.
pub fn plan_break_cycles(
    snapshot: &GraphSnapshot,
    scope: &GraphScope,
    policy: RepairPolicy,
) -> Result<RepairPlan, GraphRepairError> {
    let graph = IndexedGraph::new(snapshot, scope)?;
    let (components, membership) = graph.components();
    let mut deletions = Vec::new();
    for edge in &graph.edges {
        let source = graph.positions[&edge.source];
        let target = graph.positions[&edge.target];
        let component = membership[source];
        if source == target {
            deletions.push(RepairDeletion {
                edge: edge.id,
                reason: RepairReason::SelfLoop,
            });
        } else if component == membership[target]
            && components[component].len() > 1
            && edge.source > edge.target
        {
            deletions.push(RepairDeletion {
                edge: edge.id,
                reason: RepairReason::DescendingEdgeInComponent { component },
            });
        }
    }
    Ok(RepairPlan {
        graph: snapshot.graph.clone(),
        version: snapshot.version.clone(),
        policy,
        scope: graph.scope,
        deletions,
    })
}

/// Apply a plan as ordinary edge deletions under one local transaction lock.
///
/// The caller commits explicitly (and may set an origin or message). No policy
/// executes at receiving replicas. This is not a distributed transaction.
/// A repeated nonempty plan is stale after commit, or has pending edits before
/// commit; it cannot create extra deletions. An empty current plan is a no-op.
pub fn apply_repair(graph: &GraphHandler, plan: &RepairPlan) -> Result<usize, GraphRepairError> {
    if graph.id() != plan.graph {
        return Err(GraphRepairError::WrongGraph);
    }
    let edges: Vec<_> = plan
        .deletions
        .iter()
        .map(|deletion| deletion.edge)
        .collect();
    graph.delete_edges_if_version(&plan.version, &edges)
}

#[cfg(test)]
mod tests {
    use super::*;
    use loro_common::ContainerType;

    fn fixture(node_count: usize, edges: &[(usize, usize)]) -> GraphSnapshot {
        let nodes: Vec<_> = (0..node_count)
            .map(|n| GraphNodeId::new(7, n as i32))
            .collect();
        GraphSnapshot::new(
            ContainerID::new_root("graph", ContainerType::Graph),
            Frontiers::default(),
            nodes.clone(),
            edges
                .iter()
                .enumerate()
                .map(|(index, &(source, target))| GraphSnapshotEdge {
                    id: GraphEdgeId::new(9, index as i32),
                    source: nodes[source],
                    target: nodes[target],
                })
                .collect(),
        )
    }

    fn assert_witnesses(snapshot: &GraphSnapshot, report: &CycleReport) {
        let edges: BTreeMap<_, _> = snapshot.edges.iter().map(|e| (e.id, e)).collect();
        for component in &report.cyclic_components {
            assert!(!component.witness.is_empty());
            let witness: Vec<_> = component.witness.iter().map(|id| edges[id]).collect();
            for index in 0..witness.len() {
                assert_eq!(
                    witness[index].target,
                    witness[(index + 1) % witness.len()].source
                );
                assert!(component.nodes.contains(&witness[index].source));
            }
        }
    }

    #[test]
    fn cycle_reports_keep_multiedges_self_loops_and_disconnected_components() {
        let snapshot = fixture(
            8,
            &[
                (0, 1),
                (1, 2),
                (2, 0),
                (1, 0),
                (0, 1),
                (3, 3),
                (4, 5),
                (5, 4),
                (5, 6),
            ],
        );
        let report = analyze_cycles(&snapshot, &GraphScope::AllVisible).unwrap();
        assert_eq!(
            report.components.iter().map(Vec::len).collect::<Vec<_>>(),
            vec![3, 1, 2, 1, 1]
        );
        assert_eq!(report.cyclic_components.len(), 3);
        assert_eq!(report.self_loops, vec![GraphEdgeId::new(9, 5)]);
        assert_witnesses(&snapshot, &report);
        // Determinism includes SCCs, witnesses and the scope's canonical order.
        assert_eq!(
            report,
            analyze_cycles(&snapshot, &GraphScope::AllVisible).unwrap()
        );
        let plan = plan_break_cycles(
            &snapshot,
            &GraphScope::AllVisible,
            RepairPolicy::AscendingNodeIdV1,
        )
        .unwrap();
        assert_eq!(
            plan.deletions.iter().map(|d| d.edge).collect::<Vec<_>>(),
            vec![
                GraphEdgeId::new(9, 2),
                GraphEdgeId::new(9, 3),
                GraphEdgeId::new(9, 5),
                GraphEdgeId::new(9, 7)
            ]
        );
        let removed: BTreeSet<_> = plan.deletions.iter().map(|d| d.edge).collect();
        let mut repaired = snapshot.clone();
        repaired.edges.retain(|e| !removed.contains(&e.id));
        assert!(analyze_cycles(&repaired, &GraphScope::AllVisible)
            .unwrap()
            .is_acyclic());
        assert_eq!(
            repaired
                .edges
                .iter()
                .filter(|e| e.source == snapshot.nodes[0] && e.target == snapshot.nodes[1])
                .count(),
            2
        );
    }

    #[test]
    fn selection_is_canonical_and_never_removes_unselected_edges() {
        let snapshot = fixture(4, &[(0, 1), (1, 0), (1, 2), (2, 1), (3, 3)]);
        let a = GraphEdgeId::new(9, 0);
        let b = GraphEdgeId::new(9, 1);
        let first = plan_break_cycles(
            &snapshot,
            &GraphScope::Selected(vec![b, a, a]),
            RepairPolicy::AscendingNodeIdV1,
        )
        .unwrap();
        let second = plan_break_cycles(
            &snapshot,
            &GraphScope::Selected(vec![a, b]),
            RepairPolicy::AscendingNodeIdV1,
        )
        .unwrap();
        assert_eq!(first, second);
        assert_eq!(
            first.deletions.iter().map(|d| d.edge).collect::<Vec<_>>(),
            vec![b]
        );
        assert_eq!(first.scope(), &GraphScope::Selected(vec![a, b]));
        let empty = plan_break_cycles(
            &snapshot,
            &GraphScope::Selected(Vec::new()),
            RepairPolicy::AscendingNodeIdV1,
        )
        .unwrap();
        assert!(empty.is_empty());
        assert!(analyze_cycles(&snapshot, &GraphScope::Selected(Vec::new()))
            .unwrap()
            .is_acyclic());
        let missing = GraphEdgeId::new(10, 0);
        assert_eq!(
            analyze_cycles(&snapshot, &GraphScope::Selected(vec![missing])).unwrap_err(),
            GraphRepairError::InvalidSelection(missing)
        );
        // Analysis never filters the source snapshot in place.
        assert_eq!(snapshot.edges.len(), 5);
    }

    #[test]
    fn dag_reverse_id_edges_are_not_repaired() {
        let snapshot = fixture(4, &[(3, 2), (2, 1), (1, 0), (3, 0)]);
        assert!(analyze_cycles(&snapshot, &GraphScope::AllVisible)
            .unwrap()
            .is_acyclic());
        assert!(plan_break_cycles(
            &snapshot,
            &GraphScope::AllVisible,
            RepairPolicy::AscendingNodeIdV1
        )
        .unwrap()
        .is_empty());
    }

    #[test]
    fn deep_cycle_and_many_components_use_iterative_bounded_memory_walks() {
        let n = 20_000;
        let edges: Vec<_> = (0..n).map(|i| (i, (i + 1) % n)).collect();
        let snapshot = fixture(n, &edges);
        let report = analyze_cycles(&snapshot, &GraphScope::AllVisible).unwrap();
        assert_eq!(report.cyclic_components.len(), 1);
        assert_eq!(report.cyclic_components[0].witness.len(), n);
        let plan = plan_break_cycles(
            &snapshot,
            &GraphScope::AllVisible,
            RepairPolicy::AscendingNodeIdV1,
        )
        .unwrap();
        assert_eq!(plan.deletions.len(), 1);
        let edges: Vec<_> = (0..n).map(|i| (i, i)).collect();
        let isolated_loops = fixture(n, &edges);
        let report = analyze_cycles(&isolated_loops, &GraphScope::AllVisible).unwrap();
        assert_eq!(report.cyclic_components.len(), n);
        assert!(report
            .cyclic_components
            .iter()
            .all(|c| c.witness.len() == 1));
    }
    #[test]
    fn all_three_node_graphs_match_transitive_closure_and_repairs_are_acyclic() {
        for mask in 0u16..512 {
            let pairs: Vec<_> = (0..9)
                .filter(|bit| mask & (1 << bit) != 0)
                .map(|bit| (bit / 3, bit % 3))
                .collect();
            let snapshot = fixture(3, &pairs);
            let mut reach = [[false; 3]; 3];
            for &(a, b) in &pairs {
                reach[a][b] = true;
            }
            for mid in 0..3 {
                for a in 0..3 {
                    for b in 0..3 {
                        reach[a][b] |= reach[a][mid] && reach[mid][b];
                    }
                }
            }
            let report = analyze_cycles(&snapshot, &GraphScope::AllVisible).unwrap();
            assert_eq!(
                report.is_acyclic(),
                !(0..3).any(|n| reach[n][n]),
                "mask={mask}"
            );
            for a in 0..3 {
                for b in 0..3 {
                    let same = report.components.iter().any(|component| {
                        component.contains(&snapshot.nodes[a])
                            && component.contains(&snapshot.nodes[b])
                    });
                    assert_eq!(same, a == b || reach[a][b] && reach[b][a], "mask={mask}");
                }
            }
            assert_witnesses(&snapshot, &report);
            let plan = plan_break_cycles(
                &snapshot,
                &GraphScope::AllVisible,
                RepairPolicy::AscendingNodeIdV1,
            )
            .unwrap();
            let removed: BTreeSet<_> = plan.deletions.iter().map(|item| item.edge).collect();
            let mut indegree = [0; 3];
            let mut adjacency = [Vec::new(), Vec::new(), Vec::new()];
            for edge in &snapshot.edges {
                if removed.contains(&edge.id) {
                    continue;
                }
                let a = snapshot
                    .nodes
                    .iter()
                    .position(|n| *n == edge.source)
                    .unwrap();
                let b = snapshot
                    .nodes
                    .iter()
                    .position(|n| *n == edge.target)
                    .unwrap();
                adjacency[a].push(b);
                indegree[b] += 1;
            }
            let mut queue: VecDeque<_> = (0..3).filter(|&n| indegree[n] == 0).collect();
            let mut count = 0;
            while let Some(node) = queue.pop_front() {
                count += 1;
                for &next in &adjacency[node] {
                    indegree[next] -= 1;
                    if indegree[next] == 0 {
                        queue.push_back(next);
                    }
                }
            }
            assert_eq!(count, 3, "repair did not produce a DAG: mask={mask}");
        }
    }
}
