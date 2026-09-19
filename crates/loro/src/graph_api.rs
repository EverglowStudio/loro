//! A native directed multigraph with stable identities and remove-wins lifecycle.
use crate::{
    Container, ContainerID, ContainerTrait, Frontiers, LoroDoc, LoroMap, LoroValue, SealedTrait,
};
pub use loro_internal::container::graph::{
    GraphOrderDelta, GraphOrderError, GraphOrderTarget, GraphOrderValue, GraphPosition,
    GraphReorderOutcome, OrderedGraphEdge,
};
pub use loro_internal::graph::{GraphRepairError, GraphSnapshot};
pub use loro_internal::handler::{GraphDiff, GraphEdge, GraphNode};
pub use loro_internal::loro_common::{GraphEdgeId, GraphNodeId};
use loro_internal::{
    handler::{GraphHandler, HandlerTrait},
    LoroResult,
};
/// A native directed property multigraph. Node and edge IDs are scoped to this graph.
///
/// Cycles and multiple parents are ordinary data. Concurrent deletion wins over a
/// restore that did not observe it. Topology and metadata have independent lifecycle.
#[derive(Debug, Clone)]
pub struct LoroGraph {
    pub(crate) handler: GraphHandler,
}
impl Default for LoroGraph {
    fn default() -> Self {
        Self::new()
    }
}
impl LoroGraph {
    /// Create an empty detached graph. Attachment allocates fresh native object identities.
    pub fn new() -> Self {
        Self {
            handler: GraphHandler::new_detached(),
        }
    }
    /// Create a new isolated node with a unique operation identity and stable metadata map.
    pub fn create_node(&self) -> LoroResult<GraphNodeId> {
        self.handler.create_node()
    }
    /// Create a directed edge with immutable endpoints. Parallel edges, self-loops and cycles are valid. Both endpoint records must belong to this graph.
    pub fn create_edge(&self, source: GraphNodeId, target: GraphNodeId) -> LoroResult<GraphEdgeId> {
        self.handler.create_edge(source, target)
    }
    /// Create an edge at the local visible gap. Anchors are evaluated once under the transaction lock.
    /// Collision suffix writes use ordinary per-edge LWW, including against concurrent user moves.
    pub fn create_edge_at(
        &self,
        source: GraphNodeId,
        target: GraphNodeId,
        order: GraphOrderTarget,
    ) -> Result<GraphEdgeId, GraphOrderError> {
        self.handler.create_edge_at(source, target, order)
    }
    /// Reorder one visible edge without changing its identity, endpoints, metadata or lifecycle.
    /// Self anchors and already-satisfied positions consume no operation IDs.
    pub fn reorder_edge(
        &self,
        edge: GraphEdgeId,
        order: GraphOrderTarget,
    ) -> Result<GraphReorderOutcome, GraphOrderError> {
        self.handler.reorder_edge(edge, order)
    }
    /// Snapshot the visible outgoing sequence, preserving parallel edges.
    pub fn ordered_out_edges(&self, source: GraphNodeId) -> LoroResult<Vec<OrderedGraphEdge>> {
        self.handler.ordered_out_edges(source)
    }
    /// Select a visible outgoing edge by its current local index.
    pub fn out_edge_at(&self, source: GraphNodeId, index: usize) -> Option<OrderedGraphEdge> {
        self.handler.out_edge_at(source, index)
    }
    /// Read the rank of a visible edge within its source's sequence.
    pub fn index_of_out_edge(&self, edge: GraphEdgeId) -> Option<usize> {
        self.handler.index_of_out_edge(edge)
    }
    /// Configure local random allocation bytes (default zero); remote interpretation is deterministic.
    pub fn configure_order_jitter(&self, jitter: u8) {
        self.handler.configure_order_jitter(jitter);
    }
    /// Add a deletion tag for this node. This does not delete other nodes or edge records; incident edges become invisible while either endpoint is deleted.
    pub fn delete_node(&self, id: GraphNodeId) -> LoroResult<()> {
        self.handler.delete_node(id)
    }
    /// Delete this specific edge, retaining its endpoints, identity and metadata. Parallel edges are unaffected.
    pub fn delete_edge(&self, id: GraphEdgeId) -> LoroResult<()> {
        self.handler.delete_edge(id)
    }
    /// Restore the same node by removing its observed deletion tags. Unobserved concurrent deletes still win. Incident edges that remain alive become visible again when both endpoints are alive.
    pub fn restore_node(&self, id: GraphNodeId) -> LoroResult<()> {
        self.handler.restore_node(id)
    }
    /// Restore the same edge by removing observed deletion tags. Deleted endpoints still hide it; unobserved concurrent deletes remain effective.
    pub fn restore_edge(&self, id: GraphEdgeId) -> LoroResult<()> {
        self.handler.restore_edge(id)
    }
    /// Return visible node IDs in ascending `(peer, counter)` order.
    pub fn nodes(&self) -> Vec<GraphNodeId> {
        self.handler.nodes()
    }
    /// Return visible edge IDs in ascending `(peer, counter)` order. Edges with deleted endpoints are omitted.
    pub fn edges(&self) -> Vec<GraphEdgeId> {
        self.handler.edges()
    }
    /// Return all node records at the selected history version, including tombstones, sorted by ID.
    pub fn node_records(&self) -> Vec<GraphNode> {
        self.handler.node_records()
    }
    /// Return all edge records, including deleted and endpoint-hidden edges, sorted by ID.
    pub fn edge_records(&self) -> Vec<GraphEdge> {
        self.handler.edge_records()
    }
    /// Return the incrementally maintained number of visible nodes.
    pub fn node_count(&self) -> usize {
        self.handler.node_count()
    }
    /// Return the incrementally maintained number of visible edges.
    pub fn edge_count(&self) -> usize {
        self.handler.edge_count()
    }
    /// Return flat visible node and edge tables with metadata container references. Graph relations are ID strings, never recursive ownership links.
    pub fn get_value(&self) -> LoroValue {
        self.handler.get_value()
    }
    /// Return flat visible tables with metadata recursively resolved through normal container ownership.
    pub fn get_deep_value(&self) -> LoroValue {
        self.handler.get_deep_value()
    }
    /// Return a visible node. Use `node_record` to distinguish deletion from a missing creation.
    pub fn get_node(&self, id: GraphNodeId) -> Option<GraphNode> {
        self.handler.get_node(id)
    }
    /// Return a visible edge. Use `edge_record` for deleted or endpoint-hidden records.
    pub fn get_edge(&self, id: GraphEdgeId) -> Option<GraphEdge> {
        self.handler.get_edge(id)
    }
    /// Inspect an existing record, including `alive`, `visible`, and active deletion tags. Returns `None` for another graph or a creation outside the selected history.
    pub fn node_record(&self, id: GraphNodeId) -> Option<GraphNode> {
        self.handler.node_record(id)
    }
    /// Inspect record existence, lifecycle (`alive`) and endpoint-dependent visibility separately. `None` means no edge record exists in this graph at this version.
    pub fn edge_record(&self, id: GraphEdgeId) -> Option<GraphEdge> {
        self.handler.edge_record(id)
    }
    /// Return visible edges targeting this node, sorted by edge ID. Parallel edges are retained.
    pub fn incoming_edges(&self, id: GraphNodeId) -> LoroResult<Vec<GraphEdgeId>> {
        self.handler.incoming_edges(id)
    }
    /// Return visible edges originating at this node, sorted by edge ID. Parallel edges are retained.
    pub fn outgoing_edges(&self, id: GraphNodeId) -> LoroResult<Vec<GraphEdgeId>> {
        self.handler.outgoing_edges(id)
    }
    /// Return distinct visible incoming neighbors, sorted by node ID; parallel edges do not duplicate a neighbor.
    pub fn predecessors(&self, id: GraphNodeId) -> LoroResult<Vec<GraphNodeId>> {
        self.handler.predecessors(id)
    }
    /// Return distinct visible outgoing neighbors, sorted by node ID; parallel edges do not duplicate a neighbor.
    pub fn successors(&self, id: GraphNodeId) -> LoroResult<Vec<GraphNodeId>> {
        self.handler.successors(id)
    }
    /// Access the stable associated map, including for a deleted record. Metadata edits never restore nodes or edges.
    pub fn node_meta(&self, id: GraphNodeId) -> LoroResult<LoroMap> {
        self.handler.node_meta(id).map(LoroMap::from_handler)
    }
    /// Access the stable associated map, including for a deleted or hidden edge. Metadata edits never restore or reconnect the edge.
    pub fn edge_meta(&self, id: GraphEdgeId) -> LoroResult<LoroMap> {
        self.handler.edge_meta(id).map(LoroMap::from_handler)
    }
    /// Traverse outgoing visible edges breadth-first with visited tracking. Includes the start node and respects both depth and result-count limits.
    pub fn traverse(
        &self,
        start: GraphNodeId,
        max_depth: usize,
        max_nodes: usize,
    ) -> LoroResult<Vec<GraphNodeId>> {
        self.handler.traverse(start, max_depth, max_nodes)
    }
    /// Capture a committed DocState for pure helper analysis. Does not commit; pending edits return `UncommittedChanges`. Historical detached document states are readable.
    pub fn snapshot(&self) -> Result<GraphSnapshot, GraphRepairError> {
        self.handler.snapshot()
    }
    /// Validate the actual DocState version and all selected visible edges under the transaction lock, then generate normal DeleteEdge operations without committing. Empty input emits no operations; pending edits and stale versions are errors.
    pub fn delete_edges_if_version(
        &self,
        expected: &Frontiers,
        edges: &[GraphEdgeId],
    ) -> Result<usize, GraphRepairError> {
        self.handler.delete_edges_if_version(expected, edges)
    }
    /// Delete all currently visible nodes without cascading deletion to edge records. Explicit node restoration can therefore make old alive edges visible again.
    pub fn clear(&self) -> LoroResult<()> {
        self.handler.clear()
    }
}
impl SealedTrait for LoroGraph {}
impl ContainerTrait for LoroGraph {
    type Handler = GraphHandler;

    fn id(&self) -> ContainerID {
        self.handler.id()
    }

    fn to_container(&self) -> Container {
        Container::Graph(self.clone())
    }

    fn to_handler(&self) -> Self::Handler {
        self.handler.clone()
    }

    fn from_handler(handler: Self::Handler) -> Self {
        Self { handler }
    }

    fn is_attached(&self) -> bool {
        self.handler.is_attached()
    }

    fn get_attached(&self) -> Option<Self> {
        self.handler.get_attached().map(Self::from_handler)
    }

    fn try_from_container(container: Container) -> Option<Self> {
        container.into_graph().ok()
    }

    fn is_deleted(&self) -> bool {
        self.handler.is_deleted()
    }

    fn doc(&self) -> Option<LoroDoc> {
        self.handler.doc().map(LoroDoc::_new)
    }
}
