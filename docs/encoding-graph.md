# Native Graph encoding extension

Verified against code 2026-09-19 in the `feat/lorograph` working tree.

This document extends [the binary format reference](encoding.md) and
[the container-state reference](encoding-container-states.md) for the native
Graph container. It also specifies the Graph content of JSON updates. The
existing document envelope, checksums, ChangeStore, operation columns and
snapshot sections are unchanged. Graph uses FastUpdates mode `4`, FastSnapshot
mode `3`, and JSON `schema_version: 1`; there is no Graph-specific document mode.
This extension does not add support for legacy top-level binary modes `1` or `2`.

Source links name the implementing symbols rather than pinning line numbers
from the older references. `postcard` is locked to `1.1.3` in
[`Cargo.lock`](../Cargo.lock).

## 1. Container tags and identities

The two container-kind encodings retain their previous assignments:

| Kind | Raw tag | Historical postcard tag |
|---|---:|---:|
| Map | 0 | 1 |
| List | 1 | 2 |
| Text | 2 | 0 |
| Tree | 3 | 4 |
| MovableList | 4 | 3 |
| Counter, when enabled | 5 | 5 |
| Graph | **6** | **6** |

Raw tags occur in the change-block container arena, state keys, state-wrapper
headers and change-value container references. Historical tags occur in serde's
non-human-readable `ContainerType`, including postcard `ContainerID` values
and wrapper parents. Graph is available independently of the counter feature.
The Rust enum position is not its wire tag: `ContainerType::serialize` writes
the explicit historical byte. Existing container tags are not renumbered.

A Graph state key follows the existing `ContainerID::to_bytes()` layout:

```text
root:    86  uleb(UTF-8 name length)  UTF-8 name
normal:  06  u64le(peer)  i32le(creation_counter)
```

Here `86` and `06` are hexadecimal bytes; `0x80` is the root flag. A wrapper
for either form starts with raw kind byte `06`. A postcard ContainerID uses
the existing Root/Normal variant layout and historical kind byte `06`.

Graph nodes and edges have distinct Rust ID types, but both use the ID of
their unique creation operation: `(peer: u64, counter: i32)`. Each creation
consumes one operation counter. The associated metadata container is
`ContainerID::Normal { peer, counter, container_type: Map }` with that same
creation ID. Metadata is encoded using ordinary Map state and Map operations;
it is not embedded in a Graph operation. Node and edge creation identities
must not overlap, including across Graph containers, because their metadata
would otherwise name the same Map.

The Graph containing an operation is specified by the operation row's
container, not repeated inside the payload. Edge endpoints must name nodes
created in that Graph. Metadata Maps are children of the Graph in the
container hierarchy; graph edges are relations between node IDs, not
container-parent links.

Sources: [`loro-common/src/lib.rs`](../crates/loro-common/src/lib.rs),
`ContainerType::{to_u8,try_from_u8,serialize}`, the historical tag functions,
`ContainerID`, and `graph_id!`;
[`parent.rs`](../crates/loro-internal/src/parent.rs),
`register_container_and_parent_link`, `set_container_parent_by_raw_op`;
[`container/graph.rs`](../crates/loro-internal/src/container/graph.rs),
`GraphOp::created_meta`.

## 2. Graph operations in current binary change blocks

Every Graph operation has semantic length `1`, including Restore with any
number of delete tags. The writer emits `prop = 0` and `value_type = 11`
(`LoroValue`). Its contribution to the block's concatenated `values` section is:

```text
06                         # nested LoroValueKind::Binary, one raw byte
uleb(payload_byte_length)
postcard(GraphOp)           # exactly payload_byte_length bytes
```

The outer kind `11` belongs to `ops.value_type`; it is not repeated in
`values`. No position, key, delete-start or graph-specific side-table entry is
used by the payload. Its peer IDs are raw `u64` values, not indexes into the
change block's peer table. The enclosing change and container arena still use
their ordinary peer tables.

The reader also accepts outer `ValueKind::Binary` (`6`), whose value payload
is just `uleb(length)` followed by the same postcard bytes. The current Graph
writer emits the nested `11`/`6` form above. Other decoded value kinds are
rejected for a Graph operation. The Graph decoder does not interpret `prop`;
zero is the canonical writer value.

Sources: [`outdated_encode_reordered.rs`](../crates/loro-internal/src/encoding/outdated_encode_reordered.rs),
`get_op_prop`, `encode_op`, `decode_op` (these helpers are used by current
formats despite their filename);
[`value.rs`](../crates/loro-internal/src/encoding/value.rs), `Value::encode`,
`ValueKind`, `LoroValueKind`, `ValueWriter::write_binary`;
[`op/content.rs`](../crates/loro-internal/src/op/content.rs),
`InnerContent::content_len`.

### 2.1 Postcard grammar

`pvar(u64)` and `pvar(u32)` use unsigned base-128 varints. `pvar(i32)` uses
zigzag followed by an unsigned varint, not signed LEB128. Sequence and map
lengths use postcard's unsigned `usize` varint. Structs and fixed tuples add
no field names, field counts or length prefixes.

All three binary identity types (`GraphNodeId`, `GraphEdgeId`, `ID`) have the
same field order:

```text
ObjectOrOpID := pvar(u64 peer) pvar(i32 counter)
DeleteTags   := pvar(usize count) ObjectOrOpID[count]
GraphOp      := pvar(u32 variant_index) variant_fields
```

The variant indexes and field order are:

| Index | Rust variant | Fields, in encoded order |
|---:|---|---|
| 0 | `CreateNode` | node `id` |
| 1 | `CreateEdge` | edge `id`, node `source`, node `target` |
| 2 | `DeleteNode` | node `id` |
| 3 | `DeleteEdge` | edge `id` |
| 4 | `RestoreNode` | node `id`, `DeleteTags` |
| 5 | `RestoreEdge` | edge `id`, `DeleteTags` |

These are externally tagged serde variants. The snake-case variant names
affect JSON, not postcard; binary bytes contain the index and fields. There
is no `type` string, format-version field, document version vector, or
metadata value inside this payload.

For example, `CreateNode { id: 0@1 }` has postcard bytes `00 01 00` and
contributes `06 03 00 01 00` to `values`. A RestoreNode of `0@1` observing
delete `3@1` has postcard bytes `04 01 00 01 01 06`. Its own operation ID comes
from the enclosing change/counter, separately from both the object ID and the
delete tag.

Sources: [`container/graph.rs`](../crates/loro-internal/src/container/graph.rs),
`GraphOp`, `encoded`, `decode`, `id_vec`;
[`loro-common/src/lib.rs`](../crates/loro-common/src/lib.rs), `ID`, `graph_id!`.
Integer, enum, sequence, map and tuple rules follow postcard 1.1.3's
`src/ser/serializer.rs` and `src/de/deserializer.rs`.

### 2.2 Delete and Restore semantics

A Delete adds its own operation ID as a delete tag on the named object. A
Restore lists the specific Delete operation IDs it has observed on that same
object. A tag is inactive while at least one Restore in the selected history
names it. An object is alive when it has no active delete tags. Duplicate
tags within one Restore are rejected; a zero-length list is accepted. Restore
does not carry a document version vector.

Ordinary `restore_node`/`restore_edge` capture all currently active tags of the
object. Selective undo of a Delete emits a Restore naming only that Delete's
operation ID, so another peer's independent delete remains active. Tag order
is preserved in the vector; validation does not require sorted input.

Node deletion does not delete incident edge records. An edge is visible only
when it and both endpoints are alive. Restoring a node can reveal surviving
edges, but does not cancel an explicit edge delete. Metadata edits do not
alter this lifecycle.

Sources: [`state/graph_state.rs`](../crates/loro-internal/src/state/graph_state.rs),
`Life`, `GraphState::update_edge`;
[`handler/graph.rs`](../crates/loro-internal/src/handler/graph.rs),
`restore_node`, `restore_edge`, `apply_delta`.

## 3. FastSnapshot state and shallow snapshots

The Graph-specific bytes follow the usual ContainerWrapper header:

```text
06                         # raw Graph kind
uleb(hierarchy_depth)
postcard(Option<ContainerID>) parent
postcard(Records)           # all remaining wrapper bytes
```

There is no separate visible-value prefix, inner payload length, shared peer
table or Graph state version byte. In particular, Graph does not use the
Tree state codec just because both have associated metadata Maps.

`Records` encodes these fields in order:

```text
Records :=
    pvar(usize node_count)
    (ObjectOrOpID node_id, Life)[node_count]
    pvar(usize edge_count)
    (ObjectOrOpID edge_id,
     ObjectOrOpID source,
     ObjectOrOpID target,
     Life)[edge_count]

Life :=
    pvar(usize delete_count)
    ObjectOrOpID delete_id[delete_count]
    pvar(usize restore_count)
    (ObjectOrOpID restore_id, DeleteTags)[restore_count]
```

`nodes`, `edges` and `restores` are BTreeMaps; `deletes` is a BTreeSet. The
writer orders their entries by numeric `(peer, counter)`, not by textual ID.
The Restore value vectors retain their recorded order. An empty `Records`
payload is `00 00`.

The payload contains records for alive and deleted objects, all Delete
identities and Restore identities at the encoded version, and each Restore's
observed delete list. The cached removal reference counts (`Life::removed`)
are skipped by serde. Container indexes, incoming/outgoing adjacency,
visible sets and derived counts are also absent. Decoding validates records,
rebuilds removal counts and adjacency, and derives visibility. No attribute
Map contents are stored in `Records`.

The wrapper's Graph branch decodes this full state to derive its visible
value. `InnerStore::decode` and `decode_twice` validate Graph records in the
normal/root key ranges beginning with `06`/`86`, including a check that Graph
creation IDs do not collide across containers in that store.

The same Graph payload is used in the normal state section, shallow-root
state, and any latest-state overlay. Shallow snapshots continue to carry
`sv`/`sf`, retained change blocks and root `fr` using the base format. When
the latest-state section is the `E` sentinel, retained Graph operations are
replayed from the root state. `StateOnly` and `SnapshotAt` use their existing
snapshot export paths; neither adds a Graph-specific format.

The alive-container retention walk has a Graph branch that visits metadata
Maps for **all recorded objects**, including deleted nodes and edges, then
their retained children. A shallow export must not derive this set solely
from the visible node/edge tables: doing so loses metadata needed after
Restore. Graph relations themselves do not participate in this ownership
walk, so cycles do not cause recursive graph expansion.

Sources: [`state/graph_state.rs`](../crates/loro-internal/src/state/graph_state.rs),
`Life`, `Edge`, `Records`, `GraphState::{rebuild,decode_records}`, its
`FastStateSnapshot` implementation and `get_child_containers`;
[`container_wrapper.rs`](../crates/loro-internal/src/state/container_store/container_wrapper.rs),
`encode`, `decode_value_from_bytes`, `decode_state`;
[`inner_store.rs`](../crates/loro-internal/src/state/container_store/inner_store.rs),
`validate_graph_snapshots`;
[`state.rs`](../crates/loro-internal/src/state.rs), `get_alive_children_of`;
[`shallow_snapshot.rs`](../crates/loro-internal/src/encoding/shallow_snapshot.rs).

## 4. JSON updates and lossless IDs

JSON updates keep the existing `JsonSchema` and per-operation
`{ "container", "counter", "content" }` envelope. A root Graph named `g` is
`"cid:root-g:Graph"`. `counter` is the operation's `i32` counter. `content`
uses the native external enum shape, exported to TypeScript as `GraphJsonOp`:

```typescript
type GraphJsonOp =
    | { create_node: { id: GraphNodeId } }
    | { create_edge: { id: GraphEdgeId; source: GraphNodeId; target: GraphNodeId } }
    | { delete_node: { id: GraphNodeId } }
    | { delete_edge: { id: GraphEdgeId } }
    | { restore_node: { id: GraphNodeId; deletes: JsonOpID[] } }
    | { restore_edge: { id: GraphEdgeId; deletes: JsonOpID[] } };
```

Every identity inside Graph content is a decimal `"counter@peer"` string,
including Restore tags. Peers retain their full `u64` range, e.g.
`"0@9007199254740993"`. Parse peers as integer strings or a lossless integer
type; do not round-trip them through JavaScript `number`. Graph object
counters must be non-negative and fit `i32`. String serialization comes
from `graph_id!` and `GraphOp`'s `id_vec` adapter, not from generic ID object
serialization.

Peer compression has a deliberate boundary. The usual change IDs,
dependencies and normal container IDs are translated through `JsonSchema.peers`
when that table is present. **IDs inside Graph content remain full, original
peer IDs** in both compressed and uncompressed JSON exports; the importer
does not remap them. The `peers` table itself contains decimal strings.
Creation identity checks compare Graph content against the resolved change
peer plus the operation counter.

Example operation content (assuming the referenced object and Delete are
causal ancestors in this Graph):

```json
{
  "container": "cid:root-g:Graph",
  "counter": 4,
  "content": { "restore_node": { "id": "0@1", "deletes": ["3@1"] } }
}
```

WASM event/diff `GraphOp` uses `{ "type": "restore_node", ... }`; that is a
different API representation. Do not feed the event representation directly
to `importJsonUpdates`. A visible graph value (`nodes`/`edges` with metadata)
and the helper's `GraphSnapshot` are also different from JSON updates and
from the full CRDT state encoded in section 3.

Sources: [`json_schema.rs`](../crates/loro-internal/src/encoding/json_schema.rs),
`encode_change`, `decode_op`, `JsonOpContent`, `json_op_content_from_value`,
`validate_json_op_created_container_ids`, `serde_impl::peer_id`;
[`container/graph.rs`](../crates/loro-internal/src/container/graph.rs), `id_vec`;
[`loro-common/src/id.rs`](../crates/loro-common/src/id.rs), `ID::{fmt,try_from}`;
[`WASM graph/types.rs`](../crates/loro-wasm/src/graph/types.rs),
`GraphJsonOp` and `GraphOp` declarations.

## 5. Validation and import behavior

### 5.1 Payload and state checks

`GraphOp::decode` uses `postcard::take_from_bytes`, rejects undecodable or
truncated data and any unconsumed trailing bytes, then calls `validate`.
Validation rejects negative operation/reference counters, Create IDs that
differ from the enclosing operation ID, and repeated tags in a Restore.
JSON decoding selects the content parser from the container kind, checks
contiguous non-negative operation counters, and applies the same Graph
identity validation after resolving the change peer.

`GraphState::validate_changes` checks object type/existence in the receiving
Graph and incoming batch, disjoint creation identities, endpoint records,
and that Restore tags name Deletes of that object. Endpoint records may be
deleted: such an edge can be alive while invisible.

Snapshot `decode_records` requires exact postcard consumption. After serde
has constructed the collections, `rebuild` checks non-negative and unique
creation/Delete/Restore identities within that Graph, known node endpoints,
and distinct Restore tags present in the same object's delete set. Store
validation also checks creation-ID collisions across Graph containers.
For Graph state keys, the wrapper header is decoded fallibly and its kind must
agree with the key before state decoding. A truncated header or a complete
wrapper of another kind returns an import error through snapshot rollback.
These are record checks; direct state loading does not reconstruct the
historical causality of every Restore from the ChangeStore. The DAG check
below belongs to operation import.

### 5.2 Causal references and pending changes

`OpLog::validate_graph_import` checks imported operations against the
operation log. Each reference must have the exact ID, container, object kind
and creation/Delete operation expected by the Graph operation. A Restore
cannot name another object's Delete or a concurrent Delete merely because
the receiving document already has it. The validator walks the operation's
DAG ancestors to establish observation; it does not save a document version
vector on each Graph operation.

For references before the shallow boundary, missing retained operations are
permitted only when the shallow-start version covers the reference. Such
references are treated as observed at the root; state validation supplies
the object and delete-tag membership checks.

Operations awaiting missing change dependencies are parked as pending.
Whenever an import unlocks them, Graph validation covers those newly applied
operations as well as the incoming changes. An empty applied DAG with pending
operations is **not** eligible for direct snapshot initialization:
`OpLog::is_empty` includes the pending queue. The snapshot is imported through
the change path, allowing its history to unlock existing pending operations.
Detached or otherwise non-empty documents also use the change path; snapshot
state sections do not overwrite their existing state.

### 5.3 Rollback boundaries

Graph imports participate in the existing import rollback scopes. Graph
operations in newly applicable or pending changes require state-apply
rollback coverage. A Graph causal-validation failure rolls back the oplog
scope; a Graph state-validation failure also rejects the import rather than
publishing a partial graph. The journal includes affected pending slots,
history and arena state. Direct snapshot initialization resets state and
oplog if snapshot decoding returns an error. An attached `import_batch` or
`import_updates_batch` scope owns its batch-wide rollback rather than opening
nested scopes. A Graph causal rejection marks that scope as invalid without
consuming its journal; finalization rejects it before checkout and rolls back
the whole batch, including later imports and pending changes it unlocked.
Ordinary checksum/body decode errors keep the existing non-ACID batch contract.

These statements describe the Graph rejection paths, not a new definition
of every import error: the existing shallow-history boundary errors and
unsupported-format behavior still apply. The malformed-operation tests
check that Graph rejection leaves the previous document version/value
unchanged and that the document remains editable.

Sources: [`container/graph.rs`](../crates/loro-internal/src/container/graph.rs),
`GraphOp::{decode,validate}`, `OpLog::validate_graph_import`;
[`state/graph_state.rs`](../crates/loro-internal/src/state/graph_state.rs),
`validate_changes`, `decode_records`, `rebuild`;
[`encoding.rs`](../crates/loro-internal/src/encoding.rs),
`apply_decoded_changes_to_oplog`;
[`oplog.rs`](../crates/loro-internal/src/oplog.rs), `is_empty`,
`preflight_import_changes`, `rollback_import`;
[`pending_changes.rs`](../crates/loro-internal/src/oplog/pending_changes.rs);
[`loro.rs`](../crates/loro-internal/src/loro.rs), `can_reset_with_snapshot`,
`import_changes_and_apply_delta_to_state_if_needed`;
[`fast_snapshot.rs`](../crates/loro-internal/src/encoding/fast_snapshot.rs),
`decode_snapshot_inner`.

## 6. Readers without Graph support

The fork's pure TypeScript `loro-js` reader and MoonBit codec do not implement
native Graph operations or state. Their Graph boundary is explicit rejection:

| Reader | Graph error |
|---|---|
| `loro-js` | `LoroUnsupportedGraphError`, code `UNSUPPORTED_GRAPH`, message `unsupported Graph container (type 6)` |
| MoonBit codec | `DecodeError("unsupported Graph container (type 6)")` |

This applies to raw/historical tag `6`, including Graph IDs in container
arenas, state keys/wrappers, wrapper parents, nested container values and
JSON container references. Reader entry points inspect lazy snapshot state
and retained history so that laziness does not turn a Graph document into
a partial successful import. Generic envelope/SSTable byte parsing alone
does not imply support for interpreting Graph content.

Documents using the earlier container types keep their existing wire
assignments. This is not a claim that all previously shipped readers reject
Graph: older implementations may treat `6` as opaque Unknown. Such handling
does not provide Graph semantics or guarantee safe materialization and
re-export. A reader supporting this fork must either implement this extension
or reject Graph explicitly, without treating its bytes as another kind or
silently dropping it.

Sources: [`loro-js codec/errors.ts`](../loro-js/src/codec/errors.ts),
[`container-id.ts`](../loro-js/src/codec/container-id.ts),
[`runtime/reader-support.ts`](../loro-js/src/runtime/reader-support.ts);
[`MoonBit container_type.mbt`](../moon/loro_codec/container_type.mbt),
[`json_schema_import_ids.mbt`](../moon/loro_codec/json_schema_import_ids.mbt),
[`document.mbt`](../moon/loro_codec/document.mbt).

## 7. Regression-test map

These are source locations for the contracts above, not an execution report:

| Contract | Tests |
|---|---|
| Native operations, metadata, binary/JSON/snapshot/shallow/history, selective undo, pending snapshot merge | [`graph_native.rs`](../crates/loro/tests/graph_native.rs) |
| Creation identity, graph scope, forged concurrent Restore, truncated/trailing/invalid payload | [`graph_malformed.rs`](../crates/loro/tests/graph_malformed.rs) |
| Prior type tags and existing snapshot/update fixtures | [`graph_compat.rs`](../crates/loro/tests/graph_compat.rs) |
| Batch scope rejection and malformed Graph wrappers in full/shallow snapshots | [`import_atomicity.rs`](../crates/loro-internal/src/tests/import_atomicity.rs) |
| Independent causal model and differential history driver | [`graph_model.rs`](../crates/loro/tests/graph_model.rs) |
| WASM JSON update shape and lossless IDs | [`WASM graph.test.ts`](../crates/loro-wasm/tests/graph.test.ts) |
| Unsupported-reader boundaries | [`loro-js graph-readers.test.ts`](../loro-js/tests/graph-readers.test.ts), [`MoonBit graph_readers_test.mbt`](../moon/loro_codec/graph_readers_test.mbt) |
