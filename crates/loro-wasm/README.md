<p align="center">
  <a href="https://loro.dev">
    <picture>
      <img src="./docs/Loro.svg" width="200"/>
    </picture>
  </a>
</p>
<h1 align="center">
<a href="https://loro.dev" alt="loro-site">Loro</a>
</h1>
<p align="center">
  <b>Make your JSON data collaborative and version-controlled 🦜</b>
</p>
<p align="center">
  <a href="https://trendshift.io/repositories/4964" target="_blank"><img src="https://trendshift.io/api/badge/repositories/4964" alt="loro-dev%2Floro | Trendshift" style="width: 250px; height: 55px;" width="250" height="55"/></a>
</p>
<p align="center">
  <a href="https://loro.dev/docs">
    <b>Documentation</b>
  </a>
  |
  <a href="https://loro.dev/docs/tutorial/get_started">
    <b>Getting Started</b>
  </a>
  |
  <a href="https://docs.rs/loro">
    <b>Rust Doc</b>
  </a>
</p>
<p align="center">
  <a aria-label="X" href="https://x.com/loro_dev" target="_blank">
    <img alt="" src="https://img.shields.io/badge/Twitter-black?style=for-the-badge&logo=Twitter">
  </a>
  <a aria-label="Discord-Link" href="https://discord.gg/tUsBSVfqzf" target="_blank">
    <img alt="" src="https://img.shields.io/badge/Discord-black?style=for-the-badge&logo=discord">
  </a>
</p>


<h4 align="center">
  ✨ Loro 1.0 is out! Read the <a href="https://loro.dev/blog/v1.0">announcement</a>.
</h4>

Loro is a [CRDTs(Conflict-free Replicated Data Types)](https://crdt.tech/) library that makes building [local-first][local-first] and collaborative apps easier. You can now use it in Rust, JS (via WASM), and Swift.

# Features

**Features Provided by CRDTs**

- P2P Synchronization
- Automatic Merging
- Local Availability
- Scalability
- Delta Updates

**Supported CRDT Algorithms**

- 📝 Text Editing with [Fugue]
- 📙 [Rich Text CRDT](https://loro.dev/blog/loro-richtext)
- 🌲 [Moveable Tree](https://loro.dev/docs/tutorial/tree)
- 🚗 [Moveable List](https://loro.dev/docs/tutorial/list)
- 🗺️ [Last-Write-Wins Map](https://loro.dev/docs/tutorial/map)
- 🤝 Mergeable map-key children via `ensureMergeable*` for lazy child container creation

**Advanced Features in Loro**

- 🚀 [Fast Document Loading](https://loro.dev/blog/v1.0)
- ⏱️ Fast [Time Travel](https://loro.dev/docs/tutorial/time_travel) Through History
- 🏛️ [Version Control with Real-Time Collaboration](https://loro.dev/blog/v1.0#version-control)
- 📦 [Shallow Snapshot](https://loro.dev/docs/advanced/shallow_snapshot) that Works like Git Shallow Clone 


> In this example, we demonstrate importing an entire Loro codebase into a Loro-powered 
> version controller, preserving the complete Git DAG history while enabling fast version switching.

## Debugging the Wasm build

The standard build pipeline (`deno run -A ./scripts/build.ts dev|release`) now keeps DWARF debugging information through `wasm-bindgen` and emits two helper files alongside every `loro_wasm_bg.wasm` artifact:

- `loro_wasm_bg.wasm.map` &mdash; a v3 source map derived from DWARF so that Chrome, Edge, and Firefox can show original Rust locations when inspecting stack traces.

Load the source map in browser devtools; when devtools fetches the debug companion it can map instructions back to Rust source files and line numbers without inflating the shipped `.wasm`.

## Bundler entries

Bare `import { LoroDoc } from "loro-crdt"` uses package conditional exports. Browser development builds can resolve the nested `browser` + `development` conditions to the bundler entry, while browser production builds can resolve the `browser` condition to a synchronous browser build that avoids Vite/Rolldown production chunk cycles around `.wasm` wrappers. A legacy `browser` string field also points to the browser entry for bundlers that still consult it. Runtimes that need native `.wasm` module imports can still use the `bundler` entry, and apps that prefer explicit async initialization can use `loro-crdt/web`.

Vite and Webpack understand `new URL("./loro_wasm_bg.wasm", import.meta.url)` and emit the WASM asset automatically. Plain esbuild and plain Rollup do not copy that asset by default. For those tools, either import `loro-crdt/base64` to inline the WASM into the JS bundle without top-level await, or keep the default `loro-crdt` import and copy `node_modules/loro-crdt/browser/loro_wasm_bg.wasm` next to the emitted JS bundle as a build step.

Next.js Turbopack can use the default browser entry. If a Next.js Webpack build resolves the `bundler` entry instead of the package `browser` remap, use `loro-crdt/base64`.

# Example

[![Open in StackBlitz](https://developer.stackblitz.com/img/open_in_stackblitz.svg)](https://stackblitz.com/edit/loro-basic-test?file=test%2Floro-sync.test.ts)

```ts
import { expect, test } from 'vitest';
import { LoroDoc, LoroList } from 'loro-crdt';

test('sync example', () => {
  // Sync two docs with two rounds of exchanges

  // Initialize document A
  const docA = new LoroDoc();
  const listA: LoroList = docA.getList('list');
  listA.insert(0, 'A');
  listA.insert(1, 'B');
  listA.insert(2, 'C');

  // Export all updates from docA
  const bytes: Uint8Array = docA.export({ mode: 'update' });

  // Simulate sending `bytes` across the network to another peer, B

  const docB = new LoroDoc();
  // Peer B imports the updates from A
  docB.import(bytes);

  // B's state matches A's state
  expect(docB.toJSON()).toStrictEqual({
    list: ['A', 'B', 'C'],
  });

  // Get the current version of docB
  const version = docB.oplogVersion();

  // Simulate editing at B: delete item 'B'
  const listB: LoroList = docB.getList('list');
  listB.delete(1, 1);

  // Export the updates from B since the last sync point
  const bytesB: Uint8Array = docB.export({ mode: 'update', from: version });

  // Simulate sending `bytesB` back across the network to A

  // A imports the updates from B
  docA.import(bytesB);

  // A has the same state as B
  expect(docA.toJSON()).toStrictEqual({
    list: ['A', 'C'],
  });
});
```

# Native graphs in this fork

`LoroGraph` is a directed property multigraph container. It accepts cycles,
self-loops, parallel edges, shared nodes, and isolated nodes. Documents containing
Graph require a compatible version of this fork.

Nodes and edges have separate string ID types, `GraphNodeId` and `GraphEdgeId`.
Keep the complete `counter@peer` string: the peer is an unsigned 64-bit identity
and cannot safely be converted to a JavaScript number. Metadata uses stable
associated maps and can contain nested Text, List, Map, or Graph containers.
Edge endpoints are immutable; changing them requires deleting and creating an
edge.

```ts
import { LoroDoc, LoroText } from "loro-crdt";

const left = new LoroDoc();
left.setPeerId("1");
const graph = left.getGraph("links");
const a = graph.createNode();
const b = graph.createNode();
graph.nodeMeta(a).setContainer("text", new LoroText()).insert(0, "shared content");
left.commit();

const right = left.fork();
right.setPeerId("2");
graph.createEdge(a, b);
right.getGraph("links").createEdge(b, a);
const l = left.export({ mode: "update" });
const r = right.export({ mode: "update" });
left.import(r);
right.import(l);
// Both edges remain visible: concurrent creation of a cycle is valid.
```

Outgoing relations have an independent manual order for each source node:

```ts
const first = graph.createEdgeAt(a, b, { type: "start" });
const last = graph.createEdge(a, b); // Appends; parallel edges remain separate.
const result = graph.reorderEdge(last, { type: "before", edge: first });
console.log(result.changed, result.auxiliaryUpdates);
console.log(graph.orderedOutEdges(a)); // { edgeId, target, position }[]
console.log(graph.outEdgeAt(a, 0), graph.indexOfOutEdge(last));
left.commit(); // The methods do not commit implicitly.
```

Targets are `start`, `end`, `before`, or `after`. The latter two refer to a
visible edge in the same Graph and source at call time. A reorder preserves
identity, endpoints, and metadata. Self anchors and already-satisfied positions
return `changed: false` without allocating operation IDs. Moving a relation to
a shared child under one parent does not reorder its relations under another.

Each edge's position is an independent LWW register ordered by the operation's
Lamport/peer. Visible edges sort by position bytes and immutable edge ID, not
the last writer. Same-key insertion explicitly rewrites the necessary visible
equal-key suffix; `auxiliaryUpdates` counts those writes. They compete normally
with another replica's direct reorder, and can win or lose individually. This
does not provide range moves or a single winner for the whole batch. Optional
`configureOrderJitter(0..255)` affects only local allocation and does not remove
the need to handle collisions.

`position` is a validated hexadecimal key of 1–4096 bytes. The key grows as
needed; exhausted capacity returns `PositionTooLong` before any local write.
Order errors have a stable `code`: `MissingNode`, `EdgeNotVisible`,
`AnchorNotVisible`, `CrossSource`, `InvalidPosition`, `PositionTooLong`, or
`Engine`. Invalid JS arguments use `InvalidTarget`, `InvalidId`, `InvalidIndex`,
or `InvalidJitter`. Numeric strings, fractions, and out-of-range integers are
rejected for indices/jitter instead of being truncated.

Diagnostic edge records expose `position` and `lastOrder: { id, lamport }`;
`id` stays a full-width string. Graph diffs include per-edge `orders` with
source and before/after values. Pure order events preserve existing metadata.
Hidden edges retain their order history and use the retained winning position
when restored. Queries neither generate operations nor repair collisions.
Snapshots persist order histories but not the local jitter policy. The earlier
development Graph format without positions has no migration or dual reader.

`nodes`, `edges`, `getNode`, `getEdge`, adjacency queries, and counts describe the
visible graph. `nodeRecord`, `edgeRecord`, `nodeRecords`, and `edgeRecords` also
expose deleted records and active deletion tags. An edge can be alive but hidden
because an endpoint is deleted. Deleting a node does not delete its edge records;
restoring the node can reveal those edges again. Explicitly deleted edges stay
deleted until restored. Restoration only clears deletions the replica observed.

`predecessors` and `successors` return unique neighbors. `traverse(start,
maxDepth, maxNodes)` performs outgoing breadth-first traversal with a visited
set and explicit bounds. `toJSON`, `getShallowValue`, and `toContainerTree` use
flat node/edge tables and never recursively follow graph edges. The container
tree resolves metadata as typed container nodes, including its selected text
format.

Create a detached graph with `new LoroGraph()` and attach it through
`map.setContainer` or `list.insertContainer`. Attachment allocates new IDs; use
the returned attached graph's IDs. `parent`, `isAttached`, `getAttached`,
`isDeleted`, subscriptions, and document lookup follow the other containers.

Cycle analysis and repair are explicit:

```ts
left.commit(); // snapshot() refuses pending edits; it never commits implicitly.
const snapshot = graph.snapshot();
const report = snapshot.analyzeCycles();
const plan = snapshot.planBreakCycles();
console.log(report.cyclicComponents, plan.toJSON());

// Apply only after the caller chooses to accept the plan.
graph.applyRepair(plan);
left.commit({ origin: "explicit-graph-repair" });
```

Pass an edge-ID array to `analyzeCycles` or `planBreakCycles` to restrict the
scope; `[]` selects no edges. The default `ascending-node-id-v1` policy is
deterministic and does not minimize deletions. Snapshot and plan objects retain
their native data; editing a `toJSON()` result cannot alter a plan, and JSON
objects cannot be passed to `applyRepair`.

Applying a plan checks its graph and captured DocState version atomically before
generating ordinary edge deletions. Helper errors include a stable `code`, such
as `UncommittedChanges`, `StalePlan`, `WrongGraph`, or `InvalidSelection`. An empty
current plan writes nothing. A nonempty plan cannot be applied again while edits
are pending and becomes stale after commit. Recompute stale plans explicitly.
Import never reruns the policy, and later creation or restoration can form cycles
again. Concurrent repairs may delete more edges than a single global plan.

# Blog

- [Loro 1.0](https://loro.dev/blog/v1.0)
- [Movable tree CRDTs and Loro's implementation](https://loro.dev/blog/movable-tree)
- [Introduction to Loro's Rich Text CRDT](https://loro.dev/blog/loro-richtext)
- [Loro: Reimagine State Management with CRDTs](https://loro.dev/blog/loro-now-open-source)

# Credits

Loro draws inspiration from the innovative work of the following projects and individuals:

- [Diamond-types](https://github.com/josephg/diamond-types): The [Event Graph Walker (Eg-walker)](https://loro.dev/docs/advanced/event_graph_walker) algorithm from @josephg has been adapted to reduce the computation and space usage of CRDTs.
- [Automerge](https://github.com/automerge/automerge): Their use of columnar encoding for CRDTs has informed our strategies for efficient data encoding.
- [Yjs](https://github.com/yjs/yjs): We have incorporated a similar algorithm for effectively merging collaborative editing operations, thanks to their pioneering work.
- [Matthew Weidner](https://mattweidner.com/): His work on the [Fugue](https://arxiv.org/abs/2305.00583) algorithm has been invaluable, enhancing our text editing capabilities.
- [Martin Kleppmann](https://martin.kleppmann.com/): His work on CRDTs has significantly influenced our comprehension of the field.
 

[local-first]: https://www.inkandswitch.com/local-first/
[Fugue]: https://arxiv.org/abs/2305.00583
