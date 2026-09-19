import { describe, expect, expectTypeOf, it, vi } from "vitest";
import {
  GraphDiff,
  GraphEdgeId,
  GraphEdgeRecord,
  GraphJsonOp,
  GraphNodeId,
  GraphOrderError,
  GraphOrderTarget,
  GraphReorderOutcome,
  LoroDoc,
  LoroGraph,
  OrderedGraphEdge,
  PeerID,
  UndoManager,
} from "../bundler/index";

const MAX_PEER = "18446744073709551614";
const START = { type: "start" } as const;
const END = { type: "end" } as const;

function setup(peer: PeerID = "1") {
  const doc = new LoroDoc();
  doc.setPeerId(peer);
  const graph = doc.getGraph("graph");
  graph.configureOrderJitter(0);
  return { doc, graph };
}

function sequence(count = 4, peer: PeerID = "1") {
  const { doc, graph } = setup(peer);
  const source = graph.createNode();
  const target = graph.createNode();
  const edges = Array.from({ length: count }, () => graph.createEdge(source, target));
  doc.commit();
  return { doc, graph, source, target, edges };
}

// This is a legal JSON-op fixture, not a JS implementation of position allocation.
// Force different edges from the same full-width peer to share the exact key.
function collision(count = 4) {
  const base = sequence(count, MAX_PEER);
  const json = base.doc.exportJsonUpdates(undefined, undefined, false);
  for (const change of json.changes) {
    for (const op of change.ops) {
      const content = op.content as GraphJsonOp;
      if ("create_edge" in content) content.create_edge.position = "80";
    }
  }
  const { doc, graph } = setup("1");
  doc.importJsonUpdates(JSON.stringify(json));
  expect(graph.orderedOutEdges(base.source).map((edge) => edge.position))
    .toEqual(Array(count).fill("80"));
  return { ...base, doc, graph };
}

function ids(graph: LoroGraph, source: GraphNodeId) {
  return graph.orderedOutEdges(source).map((edge) => edge.edgeId);
}

function checkOrder(graph: LoroGraph, source: GraphNodeId, expected: GraphEdgeId[]) {
  const ordered = graph.orderedOutEdges(source);
  expect(ordered.map((edge) => edge.edgeId)).toEqual(expected);
  ordered.forEach((edge, index) => {
    expect(graph.outEdgeAt(source, index)).toEqual(edge);
    expect(graph.indexOfOutEdge(edge.edgeId)).toBe(index);
    expect(edge.position).toMatch(/^(?:[0-9a-fA-F]{2})*80$/);
    expect(edge.position.length).toBeLessThanOrEqual(8192);
  });
  expect(graph.outEdgeAt(source, expected.length)).toBeUndefined();
}

function sync(left: LoroDoc, right: LoroDoc) {
  const a = left.export({ mode: "update" });
  const b = right.export({ mode: "update" });
  left.import(b);
  right.import(a);
}

function expectCode(fn: () => unknown, code: GraphOrderError["code"]) {
  let error: unknown;
  try {
    fn();
  } catch (caught) {
    error = caught;
  }
  expect(error).toBeInstanceOf(Error);
  expect(error).toMatchObject({ name: "GraphOrderError", code });
}

function graphDiff(doc: LoroDoc, from: ReturnType<LoroDoc["frontiers"]>) {
  const batch = doc.diff(from, doc.frontiers(), true);
  const diff = batch.find(([, diff]) => diff.type === "graph")?.[1];
  expect(diff?.type).toBe("graph");
  return { batch, diff: diff as GraphDiff };
}

describe("native graph outgoing order", () => {
  it("inserts at first, last and anchored gaps and keeps parallel edges distinct", () => {
    const { doc, graph } = setup(MAX_PEER);
    const source = graph.createNode();
    const target = graph.createNode();
    checkOrder(graph, source, []);
    const b = graph.createEdge(source, target);
    const a = graph.createEdgeAt(source, target, START);
    const d = graph.createEdgeAt(source, target, END);
    const c = graph.createEdgeAt(source, target, { type: "before", edge: d });
    const middle = graph.createEdgeAt(source, target, { type: "after", edge: b });
    checkOrder(graph, source, [a, b, middle, c, d]);
    expect(graph.successors(source)).toEqual([target]);
    const metadata = graph.edgeMeta(middle);
    metadata.set("name", "same edge");
    expect(graph.reorderEdge(middle, END)).toEqual({ changed: true, auxiliaryUpdates: 0 });
    checkOrder(graph, source, [a, b, c, d, middle]);
    expect(graph.reorderEdge(d, { type: "after", edge: a }).changed).toBe(true);
    checkOrder(graph, source, [a, d, b, c, middle]);
    expect(graph.edgeMeta(middle).id).toBe(metadata.id);
    expect(graph.edgeMeta(middle).get("name")).toBe("same edge");
    expect(graph.getEdge(middle)).toMatchObject({ source, target });
    expect(middle).toMatch(`@${MAX_PEER}`);
    expect(graph.edgeRecord(middle)?.lastOrder.id).toMatch(`@${MAX_PEER}`);
    expectTypeOf(graph.orderedOutEdges(source)).toEqualTypeOf<OrderedGraphEdge[]>();
    expectTypeOf(graph.reorderEdge(a, START)).toEqualTypeOf<GraphReorderOutcome>();
    expectTypeOf(graph.outEdgeAt(source, 0)).toEqualTypeOf<OrderedGraphEdge | undefined>();
    doc.commit();
  });

  it("keeps no-ops, queries and jitter configuration free of document operations", () => {
    const { doc, graph, source, edges: [a, b, c] } = sequence(3);
    const version = doc.oplogVersion().toJSON();
    const frontiers = doc.frontiers();
    const events = vi.fn();
    const off = graph.subscribe(events);
    for (const [edge, target] of [
      [a, START], [c, END], [b, { type: "before", edge: b }],
      [b, { type: "after", edge: b }], [b, { type: "before", edge: c }],
      [b, { type: "after", edge: a }],
    ] as [GraphEdgeId, GraphOrderTarget][]) {
      expect(graph.reorderEdge(edge, target)).toEqual({ changed: false, auxiliaryUpdates: 0 });
    }
    for (const jitter of [0, 1, 255, 0]) graph.configureOrderJitter(jitter);
    checkOrder(graph, source, [a, b, c]);
    graph.edgeRecords();
    graph.toJSON();
    graph.getShallowValue();
    expect(doc.getUncommittedOpsAsJson()).toBeUndefined();
    expect(doc.oplogVersion().toJSON()).toEqual(version);
    expect(doc.frontiers()).toEqual(frontiers);
    expect(events).not.toHaveBeenCalled();
    off();

    const single = sequence(1);
    expect(single.graph.reorderEdge(single.edges[0], START).changed).toBe(false);
    expect(single.graph.reorderEdge(single.edges[0], END).changed).toBe(false);
    expect(single.doc.getUncommittedOpsAsJson()).toBeUndefined();
  });

  it("keeps multi-parent ordering independent while preserving cycles and metadata", () => {
    const { doc, graph } = setup();
    const [p, q, x, y, z] = Array.from({ length: 5 }, () => graph.createNode());
    const pEdges = [x, y, z].map((node) => graph.createEdge(p, node));
    const qEdges = [z, x, y].map((node) => graph.createEdge(q, node));
    graph.createEdge(x, p);
    graph.createEdge(x, x);
    graph.nodeMeta(x).set("content", "shared");
    doc.commit();
    const remote = doc.fork();
    remote.setPeerId("2");
    const other = remote.getGraph("graph");
    graph.reorderEdge(pEdges[0], END);
    other.reorderEdge(qEdges[2], START);
    sync(doc, remote);
    for (const g of [graph, other]) {
      checkOrder(g, p, [pEdges[1], pEdges[2], pEdges[0]]);
      checkOrder(g, q, [qEdges[2], qEdges[0], qEdges[1]]);
      expect(g.nodeMeta(x).get("content")).toBe("shared");
      expect(g.edgeCount()).toBe(8);
      expect(g.snapshot().analyzeCycles().isAcyclic).toBe(false);
    }
  });

  it("supports detached ordering and retains it when attached with remapped identities", () => {
    const detached = new LoroGraph();
    detached.configureOrderJitter(0);
    const source = detached.createNode();
    const target = detached.createNode();
    detached.nodeMeta(source).set("name", "source");
    const a = detached.createEdge(source, target);
    const b = detached.createEdge(source, target);
    detached.edgeMeta(a).set("name", "a");
    detached.edgeMeta(b).set("name", "b");
    detached.reorderEdge(b, START);
    checkOrder(detached, source, [b, a]);
    const { doc } = setup(MAX_PEER);
    const attached = doc.getMap("root").setContainer("graph", detached);
    const attachedSource = attached.nodes().find((id) => attached.nodeMeta(id).get("name") === "source")!;
    expect(attached.orderedOutEdges(attachedSource).map((edge) => attached.edgeMeta(edge.edgeId).get("name")))
      .toEqual(["b", "a"]);
    expect(attached.edges()).not.toContain(a);
  });
});

describe("graph order collisions and concurrency", () => {
  it("opens a forced equal-key gap without changing existing visible order or IDs", () => {
    const { doc, graph, source, target, edges: [a, b, c, d] } = collision();
    const beforeA = graph.edgeRecord(a);
    checkOrder(graph, source, [a, b, c, d]);
    expect(doc.getUncommittedOpsAsJson()).toBeUndefined();
    const inserted = graph.createEdgeAt(source, target, { type: "after", edge: a });
    checkOrder(graph, source, [a, inserted, b, c, d]);
    expect(new Set(graph.orderedOutEdges(source).map((edge) => edge.position)).size).toBe(5);
    expect(graph.edgeRecord(a)).toEqual(beforeA);
    expect([b, c, d].map((id) => graph.edgeRecord(id)?.lastOrder.id)).toEqual([
      "1@1", "2@1", "3@1",
    ]);
    doc.commit();
    const copy = new LoroDoc();
    copy.import(doc.export({ mode: "update" }));
    expect(copy.getGraph("graph").edgeRecords()).toEqual(graph.edgeRecords());
    checkOrder(copy.getGraph("graph"), source, [a, inserted, b, c, d]);
  });

  it("moves inside an all-equal run and excludes the moving edge before resolving its gap", () => {
    const { graph, source, edges: [a, b, c, d] } = collision();
    expect(graph.reorderEdge(d, { type: "after", edge: a })).toEqual({
      changed: true, auxiliaryUpdates: 2,
    });
    checkOrder(graph, source, [a, d, b, c]);
    expect(graph.reorderEdge(d, { type: "before", edge: b })).toEqual({
      changed: false, auxiliaryUpdates: 0,
    });
    graph.reorderEdge(a, END);
    checkOrder(graph, source, [d, b, c, a]);
  });

  it("leaves hidden equal-key records untouched while opening a visible gap", () => {
    const { graph, source, target, edges: [a, b, c] } = collision(3);
    graph.deleteEdge(b);
    const hidden = graph.edgeRecord(b);
    const inserted = graph.createEdgeAt(source, target, { type: "after", edge: a });
    checkOrder(graph, source, [a, inserted, c]);
    expect(graph.edgeRecord(b)).toEqual(hidden);
    expect(graph.indexOfOutEdge(b)).toBeUndefined();
    graph.restoreEdge(b);
    checkOrder(graph, source, [a, b, inserted, c]);
  });

  it("keeps deterministic equal keys from concurrent inserts and can keep editing", () => {
    const { doc, graph, source, target } = sequence(0);
    const remote = doc.fork();
    remote.setPeerId(MAX_PEER);
    const other = remote.getGraph("graph");
    other.configureOrderJitter(0);
    const a = graph.createEdge(source, target);
    const b = other.createEdge(source, target);
    expect(graph.edgeRecord(a)?.position).toBe(other.edgeRecord(b)?.position);
    sync(doc, remote);
    checkOrder(graph, source, [a, b]);
    checkOrder(other, source, [a, b]);
    const version = doc.oplogVersion().toJSON();
    remote.import(doc.export({ mode: "update" }));
    expect(doc.oplogVersion().toJSON()).toEqual(version);
    const middle = graph.createEdgeAt(source, target, { type: "before", edge: b });
    sync(doc, remote);
    checkOrder(other, source, [a, middle, b]);
    expect(other.edgeRecords()).toEqual(graph.edgeRecords());
  });

  it("merges concurrent gap opening and auxiliary/user writes without import repair", () => {
    const { doc, graph, source, target, edges: [a, b, c, d] } = collision();
    const remote = doc.fork();
    remote.setPeerId("2");
    const other = remote.getGraph("graph");
    const left = graph.createEdgeAt(source, target, { type: "after", edge: a });
    const right = other.createEdgeAt(source, target, { type: "after", edge: a });
    // A later independent user write beats a collision-opening write to b.
    other.reorderEdge(b, START);
    const winner = other.edgeRecord(b);
    sync(doc, remote);
    checkOrder(graph, source, [b, a, left, right, c, d]);
    expect(graph.edgeRecord(b)).toEqual(winner);
    expect(graph.edgeRecords()).toEqual(other.edgeRecords());
    const version = doc.oplogVersion().toJSON();
    const records = graph.edgeRecords();
    graph.orderedOutEdges(source);
    graph.outEdgeAt(source, 2);
    doc.import(remote.export({ mode: "update" }));
    expect(doc.oplogVersion().toJSON()).toEqual(version);
    expect(graph.edgeRecords()).toEqual(records);
    expect(doc.getUncommittedOpsAsJson()).toBeUndefined();
    const again = graph.createEdgeAt(source, target, { type: "before", edge: right });
    checkOrder(graph, source, [b, a, left, again, right, c, d]);
  });

  it("selects concurrent same-edge writes by Lamport and full peer, independently of key size", () => {
    const { doc, graph, source, edges: [a, b, c] } = sequence(3);
    const remote = doc.fork();
    remote.setPeerId(MAX_PEER);
    const other = remote.getGraph("graph");
    graph.reorderEdge(b, END);
    other.reorderEdge(b, START);
    other.edgeMeta(b).set("title", "remote metadata");
    const winning = other.edgeRecord(b);
    sync(doc, remote);
    checkOrder(graph, source, [b, a, c]);
    expect(graph.edgeRecord(b)).toEqual(winning);
    expect(graph.edgeRecord(b)?.lastOrder.id).toMatch(`@${MAX_PEER}`);
    expect(graph.edgeMeta(b).get("title")).toBe("remote metadata");
    expect(other.edgeRecords()).toEqual(graph.edgeRecords());
  });
});

describe("graph order history, events and selective undo", () => {
  it("preserves equal-key order when copy diffs allocate a new edge identity", () => {
    const { doc: seed, graph: initial } = setup("1");
    const source = initial.createNode();
    seed.commit();
    const left = seed.fork();
    left.setPeerId("2");
    const right = seed.fork();
    right.setPeerId("9");
    const l = left.getGraph("graph");
    const r = right.getGraph("graph");
    l.configureOrderJitter(0);
    r.configureOrderJitter(0);
    const a = l.createEdge(source, source);
    const b = r.createEdge(source, source);
    l.edgeMeta(a).set("marker", "A");
    r.edgeMeta(b).set("marker", "B");
    left.commit();
    right.commit();
    const v0 = right.frontiers();
    left.import(right.export({ mode: "update" }));
    checkOrder(l, source, [a, b]);
    expect(l.orderedOutEdges(source).map((edge) => edge.position)).toEqual(["80", "80"]);

    const receiver = new LoroDoc();
    receiver.setPeerId("20");
    receiver.import(right.export({ mode: "snapshot" }));
    const graph = receiver.getGraph("graph");
    const bMetaId = graph.edgeMeta(b).id;
    const bMetadata = graph.edgeMeta(b).toJSON();
    receiver.applyDiff(left.diff(v0, left.frontiers()));

    const ordered = graph.orderedOutEdges(source);
    expect(ordered.map((edge) => graph.edgeMeta(edge.edgeId).get("marker"))).toEqual(["A", "B"]);
    const copiedA = ordered[0].edgeId;
    expect(copiedA).not.toBe(a);
    expect(copiedA).toMatch(/@20$/);
    expect(graph.getEdge(a)).toBeUndefined();
    checkOrder(graph, source, [copiedA, b]);
    expect(graph.getEdge(b)).toMatchObject({ id: b, source, target: source });
    expect(graph.edgeMeta(copiedA).toJSON()).toEqual({ marker: "A" });
    expect(graph.edgeMeta(b).id).toBe(bMetaId);
    expect(graph.edgeMeta(b).toJSON()).toEqual(bMetadata);
    expect(bMetadata).toEqual({ marker: "B" });
  });

  it("round-trips order ops, Lamport, full writer IDs and before/after deltas through JS diffs", async () => {
    const { doc, graph, source, edges: [a, b, c] } = sequence(3, MAX_PEER);
    const before = doc.frontiers();
    const oldRecord = graph.edgeRecord(c)!;
    const target = doc.fork();
    target.setPeerId("2");
    const events: GraphDiff[] = [];
    const rows = new Map<GraphEdgeId, GraphEdgeRecord>(graph.edgeRecords().map((edge) => [edge.id, edge]));
    const error = vi.spyOn(console, "error").mockImplementation(() => {});
    const off = graph.subscribe((batch) => {
      for (const event of batch.events) {
        if (event.diff.type !== "graph") continue;
        events.push(event.diff);
        for (const [id, row] of Object.entries(event.diff.diff.edges)) {
          if (row === null) rows.delete(id as GraphEdgeId);
          else rows.set(id as GraphEdgeId, row);
        }
      }
    });
    try {
      graph.reorderEdge(c, START);
      doc.commit();
      const current = graph.edgeRecord(c)!;
      const { batch, diff } = graphDiff(doc, before);
      const change = diff.diff.ops.find((change) => change.op.type === "set_edge_order")!;
      expect(change.op).toEqual({ type: "set_edge_order", id: c, position: current.position });
      expect(current.lastOrder).toEqual({ id: change.id, lamport: change.lamport });
      expect(current.lastOrder.id).toMatch(`@${MAX_PEER}`);
      expect(diff.diff.orders[c]).toEqual({
        source,
        before: { position: oldRecord.position, lastOrder: oldRecord.lastOrder },
        after: { position: current.position, lastOrder: current.lastOrder },
      });
      expect(events.at(-1)?.diff.orders[c]).toEqual(diff.diff.orders[c]);
      expect([...rows.values()]).toEqual(graph.edgeRecords());
      target.applyDiff(JSON.parse(JSON.stringify(batch)));
      checkOrder(target.getGraph("graph"), source, [c, a, b]);
      expect(target.getGraph("graph").orderedOutEdges(source)).toEqual(graph.orderedOutEdges(source));
      const latest = doc.frontiers();
      doc.checkout(before);
      checkOrder(graph, source, [a, b, c]);
      expect([...rows.values()]).toEqual(graph.edgeRecords());
      doc.checkoutToLatest();
      expect(doc.frontiers()).toEqual(latest);
      expect([...rows.values()]).toEqual(graph.edgeRecords());
      const reversed = doc.diff(latest, before, true);
      const inverseTarget = doc.fork();
      inverseTarget.applyDiff(JSON.parse(JSON.stringify(reversed)));
      checkOrder(inverseTarget.getGraph("graph"), source, [a, b, c]);
      graph.createEdgeAt(source, graph.getEdge(a)!.target, END);
      doc.commit();
      await new Promise((resolve) => setTimeout(resolve, 0));
      expect(error.mock.calls.flat().join(" ")).not.toContain("[LORO_INTERNAL_ERROR] Event not called");
    } finally {
      off();
      error.mockRestore();
    }
  });

  it("preserves incoming order writes on an endpoint-hidden edge and reveals the saved position", () => {
    const { doc, graph } = setup();
    const [source, x, y, z] = Array.from({ length: 4 }, () => graph.createNode());
    const [a, b, c] = [x, y, z].map((target) => graph.createEdge(source, target));
    doc.commit();
    const remote = doc.fork();
    remote.setPeerId("2");
    const other = remote.getGraph("graph");
    graph.deleteNode(y);
    other.reorderEdge(b, START);
    other.edgeMeta(b).set("hidden", "retained");
    const saved = other.edgeRecord(b)!;
    sync(doc, remote);
    checkOrder(graph, source, [a, c]);
    expect(graph.getEdge(b)).toBeUndefined();
    expect(graph.indexOfOutEdge(b)).toBeUndefined();
    expect(graph.edgeRecord(b)).toMatchObject({
      alive: true, visible: false, position: saved.position, lastOrder: saved.lastOrder,
    });
    graph.restoreNode(y);
    checkOrder(graph, source, [b, a, c]);
    expect(graph.edgeMeta(b).get("hidden")).toBe("retained");
    graph.deleteEdge(b);
    expectCode(() => graph.reorderEdge(b, END), "EdgeNotVisible");
    graph.restoreEdge(b);
    checkOrder(graph, source, [b, a, c]);
  });

  it("round-trips binary/full/shallow and compressed/uncompressed JSON and continues editing", () => {
    const { doc, graph, source, target, edges: [a, b, c] } = sequence(3, MAX_PEER);
    const base = doc.frontiers();
    graph.reorderEdge(c, START);
    doc.commit();
    const expected = graph.edgeRecords();
    const copies: LoroDoc[] = [];
    for (const mode of [
      { mode: "snapshot" }, { mode: "update" }, { mode: "shallow-snapshot", frontiers: base },
    ] as const) {
      const copy = new LoroDoc();
      const bytes = doc.export(mode);
      copy.import(bytes);
      copy.import(bytes);
      copies.push(copy);
    }
    for (const compressed of [false, true]) {
      const json = doc.exportJsonUpdates(undefined, undefined, compressed);
      const copy = new LoroDoc();
      copy.importJsonUpdates(JSON.stringify(json));
      copies.push(copy);
    }
    for (const copy of copies) {
      copy.setPeerId("17");
      const g = copy.getGraph("graph");
      expect(g.edgeRecords()).toEqual(expected);
      checkOrder(g, source, [c, a, b]);
      const inserted = g.createEdgeAt(source, target, { type: "after", edge: a });
      checkOrder(g, source, [c, a, inserted, b]);
      const reloaded = new LoroDoc();
      reloaded.import(copy.export({ mode: "snapshot" }));
      expect(reloaded.getGraph("graph").orderedOutEdges(source)).toEqual(g.orderedOutEdges(source));
    }
  });

  it("buffers ordering updates with missing causal dependencies and replays duplicates without writes", () => {
    const { doc, graph, source, edges: [a, b, c] } = sequence(3);
    const baseBytes = doc.export({ mode: "snapshot" });
    const base = doc.oplogVersion();
    graph.reorderEdge(c, START);
    const delta = doc.export({ mode: "update", from: base });
    const { doc: remote, graph: other } = setup("2");
    remote.import(delta);
    expect(other.edges()).toEqual([]);
    remote.import(baseBytes);
    checkOrder(other, source, [c, a, b]);
    const version = remote.oplogVersion().toJSON();
    remote.import(delta);
    expect(remote.oplogVersion().toJSON()).toEqual(version);
    expect(remote.getUncommittedOpsAsJson()).toBeUndefined();
  });

  it("reads historical order without enabling edits or allocating operations", () => {
    const { doc, graph, source, target, edges: [a, b, c] } = sequence(3);
    const historical = doc.frontiers();
    graph.reorderEdge(c, START);
    doc.commit();
    const version = doc.oplogVersion().toJSON();
    doc.checkout(historical);
    checkOrder(graph, source, [a, b, c]);
    expectCode(() => graph.reorderEdge(c, START), "Engine");
    expectCode(() => graph.createEdgeAt(source, target, END), "Engine");
    expect(doc.frontiers()).toEqual(historical);
    expect(doc.oplogVersion().toJSON()).toEqual(version);
    expect(doc.getUncommittedOpsAsJson()).toBeUndefined();
    doc.checkoutToLatest();
    checkOrder(graph, source, [c, a, b]);
  });

  it("undoes and redoes one move without replacing identity or metadata", () => {
    const { doc, graph, source, edges: [a, b, c] } = sequence(3);
    graph.edgeMeta(c).set("title", "kept");
    doc.commit();
    const metaId = graph.edgeMeta(c).id;
    const undo = new UndoManager(doc, { mergeInterval: 0 });
    graph.reorderEdge(c, START);
    doc.commit();
    checkOrder(graph, source, [c, a, b]);
    undo.undo();
    checkOrder(graph, source, [a, b, c]);
    undo.redo();
    checkOrder(graph, source, [c, a, b]);
    expect(graph.edgeMeta(c).id).toBe(metaId);
    expect(graph.edgeMeta(c).get("title")).toBe("kept");
  });

  it("undoes a local move while retaining a different edge's remote move and metadata", () => {
    const { doc, graph, source, edges: [a, b, c, d] } = sequence();
    const remote = doc.fork();
    remote.setPeerId("2");
    const other = remote.getGraph("graph");
    const undo = new UndoManager(doc, { mergeInterval: 0 });
    graph.reorderEdge(d, START);
    doc.commit();
    other.reorderEdge(c, START);
    other.edgeMeta(d).set("title", "remote");
    const remoteC = other.edgeRecord(c);
    sync(doc, remote);
    undo.undo();
    checkOrder(graph, source, [c, a, b, d]);
    expect(graph.edgeRecord(c)).toEqual(remoteC);
    expect(graph.edgeMeta(d).get("title")).toBe("remote");
    undo.redo();
    expect(graph.edgeRecord(c)).toEqual(remoteC);
    expect(graph.edgeMeta(d).get("title")).toBe("remote");
  });

  it("does not undo a winning remote write to the same edge", () => {
    const { doc, graph, source, edges: [a, b, c] } = sequence(3);
    const remote = doc.fork();
    remote.setPeerId(MAX_PEER);
    const other = remote.getGraph("graph");
    const undo = new UndoManager(doc, { mergeInterval: 0 });
    graph.reorderEdge(b, START);
    doc.commit();
    other.reorderEdge(b, END);
    const saved = other.edgeRecord(b);
    sync(doc, remote);
    checkOrder(graph, source, [a, c, b]);
    undo.undo();
    checkOrder(graph, source, [a, c, b]);
    expect(graph.edgeRecord(b)).toEqual(saved);
  });

  it("undoes collision-opening auxiliary writes selectively after a remote neighbor move", () => {
    const { doc, graph, source, edges: [a, b, c, d] } = collision();
    const remote = doc.fork();
    remote.setPeerId("2");
    const other = remote.getGraph("graph");
    const undo = new UndoManager(doc, { mergeInterval: 0 });
    expect(graph.reorderEdge(d, { type: "after", edge: a }).auxiliaryUpdates).toBe(2);
    doc.commit();
    for (let i = 0; i < 3; i++) other.edgeMeta(b).set("clock", i);
    other.reorderEdge(b, START);
    const saved = other.edgeRecord(b);
    sync(doc, remote);
    undo.undo();
    checkOrder(graph, source, [b, a, c, d]);
    expect(graph.edgeRecord(b)).toEqual(saved);
    expect(graph.edgeRecord(c)?.position).toBe("80");
    expect(graph.edgeRecord(d)?.position).toBe("80");
    expect(graph.edgeMeta(b).get("clock")).toBe(2);
  });
});

describe("graph order boundary validation", () => {
  it("rejects invalid targets, IDs, indexes and jitter before consuming an operation", () => {
    const { doc, graph, source, target, edges: [a, b, c] } = sequence(3);
    const foreign = doc.getGraph("other");
    const node = foreign.createNode();
    const foreignEdge = foreign.createEdge(node, node);
    const anotherSource = graph.createNode();
    const crossSource = graph.createEdge(anotherSource, target);
    doc.commit();
    const version = doc.oplogVersion().toJSON();
    for (const invalid of [null, undefined, "start", 0, [], {}, { type: "middle" },
      { type: "before" }, { type: "after", edge: 1 }, { type: "start", edge: a }]) {
      expectCode(() => graph.reorderEdge(a, invalid as GraphOrderTarget), "InvalidTarget");
    }
    for (const invalid of ["", "x@1", "-1@1", "0@18446744073709551616", "2147483648@1", 1, null]) {
      expectCode(() => graph.reorderEdge(invalid as GraphEdgeId, START), "InvalidId");
      expectCode(() => graph.createEdgeAt(invalid as GraphNodeId, target, END), "InvalidId");
    }
    expectCode(() => graph.reorderEdge(a, { type: "before", edge: "bad" as GraphEdgeId }), "InvalidId");
    expectCode(() => graph.reorderEdge(a, { type: "before", edge: crossSource }), "CrossSource");
    expectCode(() => graph.createEdgeAt(source, target, { type: "after", edge: crossSource }), "CrossSource");
    expectCode(() => graph.reorderEdge(a, { type: "after", edge: foreignEdge }), "AnchorNotVisible");
    expectCode(() => graph.reorderEdge(foreignEdge, END), "EdgeNotVisible");
    expectCode(() => graph.createEdgeAt(source, node, END), "MissingNode");
    for (const invalid of [-1, 0.5, NaN, Infinity, 2 ** 32, "0", null]) {
      expectCode(() => graph.outEdgeAt(source, invalid as number), "InvalidIndex");
    }
    for (const invalid of [-1, 0.5, NaN, Infinity, 256, "0", null]) {
      expectCode(() => graph.configureOrderJitter(invalid as number), "InvalidJitter");
    }
    expect(doc.oplogVersion().toJSON()).toEqual(version);
    expect(doc.getUncommittedOpsAsJson()).toBeUndefined();
    checkOrder(graph, source, [a, b, c]);
    graph.deleteEdge(b);
    const pending = doc.getUncommittedOpsAsJson();
    expectCode(() => graph.reorderEdge(a, { type: "before", edge: b }), "AnchorNotVisible");
    expect(doc.getUncommittedOpsAsJson()).toEqual(pending);
    const valid = graph.createEdgeAt(source, target, START);
    checkOrder(graph, source, [valid, a, c]);
  });

  it("rejects malformed and oversized positions from JSON without corrupting the WASM instance", () => {
    const { doc } = sequence(1, MAX_PEER);
    const original = doc.exportJsonUpdates(undefined, undefined, false);
    for (const position of ["", "0", "7f", "0081", "g080", "80".repeat(4097)]) {
      const json = JSON.parse(JSON.stringify(original)) as typeof original;
      for (const change of json.changes) for (const op of change.ops) {
        const content = op.content as GraphJsonOp;
        if ("create_edge" in content) content.create_edge.position = position;
      }
      const copy = new LoroDoc();
      expect(() => copy.importJsonUpdates(JSON.stringify(json))).toThrow();
      const fresh = setup("9");
      const node = fresh.graph.createNode();
      const edge = fresh.graph.createEdgeAt(node, node, END);
      checkOrder(fresh.graph, node, [edge]);
    }
  });

  it("preserves a maximum-length canonical key and full-width IDs across JSON and binary", () => {
    const { doc, source, edges: [edge] } = sequence(1, MAX_PEER);
    const json = doc.exportJsonUpdates(undefined, undefined, false);
    const position = "01".repeat(4095) + "80";
    for (const change of json.changes) for (const op of change.ops) {
      const content = op.content as GraphJsonOp;
      if ("create_edge" in content) content.create_edge.position = position;
    }
    const copy = new LoroDoc();
    copy.importJsonUpdates(JSON.stringify(json));
    const graph = copy.getGraph("graph");
    expect(graph.edgeRecord(edge)?.position).toBe(position);
    expect(graph.edgeRecord(edge)?.lastOrder.id).toBe(edge);
    checkOrder(graph, source, [edge]);
    const reloaded = new LoroDoc();
    reloaded.import(copy.export({ mode: "snapshot" }));
    expect(reloaded.getGraph("graph").edgeRecords()).toEqual(graph.edgeRecords());
    checkOrder(reloaded.getGraph("graph"), source, [edge]);
  });

  it("rejects invalid diff Lamport, writer and position fields on every conversion path", () => {
    const { doc, graph, edges: [, , c] } = sequence(3, MAX_PEER);
    const before = doc.frontiers();
    graph.reorderEdge(c, START);
    doc.commit();
    const { batch } = graphDiff(doc, before);
    const edits: ((diff: GraphDiff["diff"]) => void)[] = [
      (diff) => { diff.ops[0].lamport = -1; },
      (diff) => { diff.ops[0].lamport = 2 ** 32; },
      (diff) => { diff.ops[0].lamport = 1.5; },
      (diff) => { diff.edges[c]!.lastOrder.id = "0@18446744073709551616"; },
      (diff) => { diff.edges[c]!.lastOrder.lamport = -1; },
      (diff) => { diff.edges[c]!.position = "00"; },
      (diff) => { diff.orders[c].after!.position = "80".repeat(4097); },
      (diff) => { diff.orders[c].before!.lastOrder.id = "-1@1"; },
      (diff) => {
        const op = diff.ops.find((change) => change.op.type === "set_edge_order")!.op;
        if (op.type === "set_edge_order") op.position = "";
      },
    ];
    for (const edit of edits) {
      const invalid = JSON.parse(JSON.stringify(batch)) as typeof batch;
      const diff = invalid.find(([, diff]) => diff.type === "graph")![1] as GraphDiff;
      edit(diff.diff);
      const receiver = doc.fork();
      const version = receiver.oplogVersion().toJSON();
      expect(() => receiver.applyDiff(invalid)).toThrow();
      expect(receiver.oplogVersion().toJSON()).toEqual(version);
      expect(receiver.getUncommittedOpsAsJson()).toBeUndefined();
      expect(receiver.getGraph("graph").edgeRecords()).toEqual(graph.edgeRecords());
    }
  });
});
