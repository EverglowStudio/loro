# Native LoroGraph context

Verified against code 2026-09-19.

LoroGraph is a native directed multigraph container. Cycles, self-loops,
parallel edges and multiple incoming parents are valid CRDT state. Optional
cycle analysis and repair are separate from replication and merge. The
[encoding extension](../docs/encoding-graph.md) is the source for byte layout,
JSON shapes and unsupported-reader behavior; do not infer Graph's format
from Map or Tree codecs.

## Code map

| Concern | Source and entry points |
|---|---|
| Public API and semantic rustdoc | [`crates/loro/src/graph_api.rs`](../crates/loro/src/graph_api.rs), `LoroGraph` |
| IDs and kind tags | [`loro-common/src/lib.rs`](../crates/loro-common/src/lib.rs), `graph_id!`, `ContainerType` |
| Operation format and import causality | [`container/graph.rs`](../crates/loro-internal/src/container/graph.rs), `GraphOp`, `OpLog::validate_graph_import` |
| Local mutation, metadata, undo and atomic helper entry | [`handler/graph.rs`](../crates/loro-internal/src/handler/graph.rs), `GraphHandler` |
| Lifecycle, adjacency, state codec | [`state/graph_state.rs`](../crates/loro-internal/src/state/graph_state.rs), `Life`, `GraphState` |
| History and checkout diff calculation | [`diff_calc/graph.rs`](../crates/loro-internal/src/diff_calc/graph.rs), `GraphDiffCalculator` |
| Graph event conversion | [`internal event.rs`](../crates/loro-internal/src/event.rs), [`public event.rs`](../crates/loro/src/event.rs), `Diff::Graph` |
| Pure analysis and explicit repair | [`internal graph.rs`](../crates/loro-internal/src/graph.rs), `GraphSnapshot`, `analyze_cycles`, `plan_break_cycles`, `apply_repair` |
| WASM API and JSON/event type declarations | [`WASM graph.rs`](../crates/loro-wasm/src/graph.rs), [`graph/types.rs`](../crates/loro-wasm/src/graph/types.rs) |

The public crate re-exports the helper module as `loro::graph`. WASM consumes
the internal crate directly; common helper algorithms belong in the internal
module, not only in the public wrapper.

## Record, alive and visible are separate concepts

`node_record`/`edge_record` return a record whenever creation exists in the
current selected history. `get_node`/`get_edge` return visible objects only.
An object is alive when every recorded Delete tag is covered by a recorded
Restore. Nodes are visible when alive. Edges additionally require both
endpoints to be alive. Checkout before creation removes the record itself.

`delete_node` changes only the node lifecycle. It hides incident edges without
adding edge Delete operations. `restore_node` can reveal those surviving edges;
an explicitly deleted edge needs its own Restore. `clear` deletes the visible
nodes and retains graph records, metadata and edge lifecycles.

Creation takes a unique operation ID; node/edge IDs are ordered numerically
by `(peer, counter)`. `nodes`/`edges` return visible IDs in that order.
`incoming_edges`/`outgoing_edges` preserve parallel edges, while
`predecessors`/`successors` deduplicate neighboring nodes. Object IDs are
validated against the Graph that receives an operation.

`ordered_out_edges(source)` is a separate source-scoped sequence of relations,
including parallel edges. Creation appends by default; `create_edge_at` and
`reorder_edge` evaluate Start/End/Before/After once under the transaction lock.
No-op moves consume no operation IDs. Anchors must be visible in this Graph
and share the source; endpoints are immutable. Queries do not commit or repair.

Each edge retains position writes, including hidden and losing writes. The
winner is maximal `(Lamport, peer)`; display order is `(GraphPosition bytes,
immutable EdgeId(peer,counter))`. Never use the last writer as a display tie.
`container/graph/order.rs` validates 1–4096 byte keys ending in `80`; it wraps
the existing fractional-index generator and keeps Tree's format unchanged.
Equal-key gaps cause explicit ordinary LWW writes to the necessary visible
suffix. Concurrent direct moves can win against some of these writes. This
is single-edge movement with auxiliary writes, not range movement.

`state/graph_state/order_index.rs` reuses the existing generic B-tree's length
caches and leaf handles for rank/select. Only visible edges occupy the index;
restore rebuilds their entries using retained positions. The whole outgoing
snapshot allocates O(d) output; rank/select use cached subtree lengths. A
collision plan additionally costs its affected suffix and generated keys.

`node_meta`/`edge_meta` return the associated normal Map at the creation ID.
Metadata stays addressable on deleted records, and writing it does not restore
the graph object. Graph topology is not encoded as Map properties. Deep values
contain flat node and edge tables; only metadata container references are
expanded through the document hierarchy.

The state keeps incoming/outgoing adjacency and visible sets incrementally;
a node lifecycle change reevaluates its incident edges. `Life::alive` and
`Life::active` still scan the object's historical Delete set, so do not claim
constant-time lifecycle checks regardless of history size.

## Metadata event paths and revival

`GraphState::get_child_index` uses the existing `Index::Node(TreeID { peer,
counter })` for both node and edge associated metadata Maps. Here `TreeID`
only carries the object's creation identity; it does not imply Tree topology
or Tree ownership. Graph endpoints never become container owner links.

[`ApplyDiff::apply`](../crates/loro-internal/src/value.rs) interprets that
semantic index within the current container's projection: a Graph searches
its flat `nodes` and `edges` rows by identity and enters the matching `meta`.
Ordinary Maps continue to use `Index::Key`; user fields named `nodes`, `edges`
or a ContainerID are not enough to identify a Graph. Hidden-record metadata
remains addressable, but a visible-value mirror ignores its path while the
corresponding row is absent.

Lifecycle record upserts in `Diff::Graph` rebuild the row's metadata slot.
[`DocState::diffs_to_event`](../crates/loro-internal/src/state.rs) supplies the
full metadata subtree after event composition, replacing intermediate child
diffs so nested Text/List contents are not inserted twice. The same file's
`trigger_on_new_container` propagates revival through associated Maps and
their nested containers. This covers local Restore and checkout bringing
back a Graph removed from a parent Map, even without intervening metadata
operations. The tradeoff is full subtree events for changed visible records;
propagation follows container ownership, never user graph relationships.
Pure order changes patch the position while retaining the row's existing
metadata, and do not request a full metadata subtree. Edge value rows carry
position; diagnostic records and order deltas also expose the selected writer.

`DocState::start_recording_for_edit` is used by public `diff` and selective
undo. In that mode, Graph lifecycle changes do not request full metadata:
their associated Maps already exist, and copying unchanged historical values
would overwrite concurrent properties. Actual Map/Text changes remain ordinary
diffs. Forward Graph creations still bring their metadata subtree so applying
the diff can copy it under freshly remapped object IDs. Subscriber recording
continues to use the full visible-row reconstruction described above.

## Restore and selective undo

A Restore carries the exact observed Delete operation IDs for its object.
Concurrent unobserved Deletes remain active. Public Restore captures the
currently active tags; undoing a Delete emits a Restore with only the original
Delete ID. `GraphChange` retains operation identity and direction for history
retreat and undo. Removing a Restore during checkout decrements its tag
reference counts rather than erasing another Restore of the same tag.

Public `apply_diff`/`revert_to` generate new local edits for the net effect of
the complete graph diff; they do not copy the source's operation history.
[`graph_state/local_diff.rs`](../crates/loro-internal/src/state/graph_state/local_diff.rs)
interprets lifecycle changes on copies of the affected objects' `Life` records.
The resulting Create/Delete/Restore plan preserves unrelated remote Deletes
and other Restores. Creation gets new object IDs, and `GraphHandler::apply_delta`
remaps node/edge references and their metadata ContainerIDs. This planning step
is separate from checkout's exact forward/backward history application and
does not change the wire format.

Fresh edge identities can change a position's tie-break when a diff copies
only part of a source's outgoing edges. The
[`copy_order` planner](../crates/loro-internal/src/state/graph_state/local_diff/copy_order.rs)
fixes the future visible sequence using net lifecycles and planned order edits,
then allocates keys for copied prefixes and necessary mixed suffixes. Groups
are identified by their original positions; the next group's copied prefix
follows the preceding group's final allocated key. All allocations succeed
before changing the edit plan. Existing order writes are replaced by actual
destination identity, so auxiliary planning cannot emit duplicate writes.
This preserves copy order without changing remote merge or identity ordering.

Order edits use `GraphDiff::orders`, a per-edge net before/after position and
writer, separately from exact historical `ops`. Composition retains earliest
before and final after. Editable application emits fresh writes, so a previous
Undo's compensation writer does not prevent a later Undo. Transform protects
independent winning order writes; `DiffBatch::transform_for_undo` passes the
entire selected ID spans, including groups split by independent remote Map
dependencies. Peer equality alone cannot identify the group. Graph diff apply
errors propagate through Undo rather than reporting false success.

The import validator proves observation from the operation DAG. Merely finding
a Delete in the receiving document does not prove the Restore observed it.
Shallow imports combine the retained boundary with record validation; see
[encoding validation](../docs/encoding-graph.md#5-validation-and-import-behavior).

## Helper snapshot and apply boundary

`GraphHandler::snapshot()` captures the Graph container ID, `DocState.frontiers`,
visible nodes and visible edge endpoints while holding the document's
transaction lock and state lock. It has no attributes, does not implicitly
commit, and returns `UncommittedChanges` if the local transaction is nonempty.
An unattached container returns `DetachedContainer`; an attached Graph in a
detached historical document can be read.

`delete_edges_if_version(expected: &Frontiers, edges: &[GraphEdgeId])` uses the
same transaction lock as the document import/checkout barrier. It checks
editability, an empty transaction, the current **DocState** version, container
liveness and every selected edge's visibility before generating ordinary
DeleteEdge operations. Duplicate selections are deduplicated. It does not
commit. An empty valid selection writes no operations.

`graph::apply_repair` checks the plan's container ID and delegates to that
version-checked entry. The helper snapshot is not a FastSnapshot or a CRDT
serialization, and the version must not be substituted with the latest
OpLog frontiers: those can differ during historical checkout.

## Focused regressions

[`graph_native.rs`](../crates/loro/tests/graph_native.rs) covers native lifecycle,
metadata, transport/history, selective undo and pending snapshot merge.
[`graph_model.rs`](../crates/loro/tests/graph_model.rs) provides the independent
causal model and differential driver.
[`graph_malformed.rs`](../crates/loro/tests/graph_malformed.rs) covers invalid
identities/references, causality and payload rejection.
[`graph_events.rs`](../crates/loro/tests/graph_events.rs) rebuilds a deep-value
mirror solely from public event paths/diffs, covering metadata edits, nested
Text, hidden-record restoration, parent-Map checkout and ordinary Map fields.
[`graph_helpers.rs`](../crates/loro/tests/graph_helpers.rs) covers helper purity
and repair version checks; [`graph_compat.rs`](../crates/loro/tests/graph_compat.rs)
covers earlier type tags and fixtures. Test presence is not an execution result.
