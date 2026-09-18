import { describe, expect, expectTypeOf, it, vi } from "vitest";
import {
  Container,
  getType,
  GraphDiff,
  GraphEdgeId,
  GraphEdgeRecord,
  GraphJsonOp,
  GraphNodeId,
  GraphNodeRecord,
  GraphRepairError,
  GraphRepairPlan,
  isContainer,
  isContainerId,
  LoroDoc,
  LoroGraph,
  LoroText,
  newContainerID,
  newRootContainerID,
  PeerID,
  UndoManager,
} from "../bundler/index";

// u64::MAX is reserved for detached/internal IDs; the preceding peer is legal
// and still exercises the full-width IDs that JS numbers cannot represent.
const MAX_PEER = "18446744073709551614";

function setup(peer: PeerID = "1") {
  const doc = new LoroDoc();
  doc.setPeerId(peer);
  return { doc, graph: doc.getGraph("graph") };
}

function errorCode(fn: () => unknown, code: GraphRepairError["code"]) {
  let error: unknown;
  try {
    fn();
  } catch (caught) {
    error = caught;
  }
  expect(error).toBeInstanceOf(Error);
  expect((error as GraphRepairError).code).toBe(code);
}

describe("native graph", () => {
  it("preserves shared nodes, diamonds, cycles, self-loops, parallel edges and isolated nodes", () => {
    const { graph } = setup();
    const [a, b, c, shared, isolated] = Array.from(
      { length: 5 },
      () => graph.createNode(),
    );
    const ab = graph.createEdge(a, b);
    const ac = graph.createEdge(a, c);
    const bs = graph.createEdge(b, shared);
    const cs = graph.createEdge(c, shared);
    const parallel = graph.createEdge(b, shared);
    const back = graph.createEdge(shared, a);
    const loop = graph.createEdge(shared, shared);
    graph.nodeMeta(shared).set("name", "one shared node");
    expect(graph.nodes()).toEqual([a, b, c, shared, isolated]);
    expect(graph.edges()).toEqual([ab, ac, bs, cs, parallel, back, loop]);
    expect(graph.nodeCount()).toBe(5);
    expect(graph.edgeCount()).toBe(7);
    expect(graph.incomingEdges(shared)).toEqual([bs, cs, parallel, loop]);
    expect(graph.outgoingEdges(a)).toEqual([ab, ac]);
    expect(graph.predecessors(shared)).toEqual([b, c, shared]);
    expect(graph.successors(b)).toEqual([shared]);
    expect(graph.nodeMeta(graph.successors(b)[0]).id).toBe(
      graph.nodeMeta(graph.successors(c)[0]).id,
    );
    expect(graph.traverse(a, 0, 20)).toEqual([a]);
    expect(graph.traverse(a, 1, 20)).toEqual([a, b, c]);
    expect(graph.traverse(a, 20, 20)).toEqual([a, b, c, shared]);
    expect(graph.traverse(a, 20, 2)).toEqual([a, b]);
    expect(graph.traverse(a, 20, 0)).toEqual([]);
    for (const invalid of [-1, 0.5, Infinity, NaN, 2 ** 32]) {
      expect(() => graph.traverse(a, invalid, 5)).toThrow();
      expect(() => graph.traverse(a, 5, invalid)).toThrow();
    }
  });

  it("distinguishes record existence, lifecycle and endpoint visibility", () => {
    const { graph } = setup();
    const a = graph.createNode();
    const b = graph.createNode();
    const kept = graph.createEdge(a, b);
    const removed = graph.createEdge(a, b);
    graph.edgeMeta(kept).set("label", "survives endpoint deletion");
    graph.deleteEdge(removed);
    graph.deleteNode(a);
    expect(graph.getNode(a)).toBeUndefined();
    expect(graph.nodeRecord(a)).toMatchObject({
      id: a,
      alive: false,
      visible: false,
    });
    expect(graph.getEdge(kept)).toBeUndefined();
    expect(graph.edgeRecord(kept)).toMatchObject({
      alive: true,
      visible: false,
      deleteTags: [],
    });
    expect(graph.nodeRecords()).toHaveLength(2);
    expect(graph.edgeRecords()).toHaveLength(2);
    expect(graph.edges()).toEqual([]);
    expect(graph.incomingEdges(b)).toEqual([]);
    graph.nodeMeta(a).set("editedWhileDeleted", true);
    expect(graph.getNode(a)).toBeUndefined();
    graph.restoreNode(a);
    expect(graph.edges()).toEqual([kept]);
    expect(graph.edgeMeta(kept).get("label")).toBe(
      "survives endpoint deletion",
    );
    graph.restoreEdge(removed);
    expect(graph.edges()).toEqual([kept, removed]);
    expect(graph.nodeMeta(a).get("editedWhileDeleted")).toBe(true);
    expect(graph.nodeMeta(a).id).not.toBe(graph.edgeMeta(kept).id);
  });

  it("rejects malformed and foreign IDs without trapping subsequent operations", () => {
    const { doc, graph } = setup();
    const local = graph.createNode();
    const foreign = doc.getGraph("other").createNode();
    for (
      const value of [
        foreign,
        "0@0",
        "-1@1",
        "0@18446744073709551616",
        "bad",
        1,
        null,
      ]
    ) {
      expect(() => graph.createEdge(local, value as GraphNodeId)).toThrow();
      expect(() => graph.nodeMeta(value as GraphNodeId)).toThrow();
    }
    expect(() => graph.deleteEdge(local as unknown as GraphEdgeId)).toThrow();
    expect(graph.nodeRecord(foreign)).toBeUndefined();
    const next = graph.createNode();
    expect(graph.getEdge(graph.createEdge(local, next))).toMatchObject({
      source: local,
      target: next,
    });
  });

  it("supports root, nested, detached and dynamically resolved containers", () => {
    const { doc, graph } = setup(MAX_PEER);
    const detached = new LoroGraph();
    const first = detached.createNode();
    const second = detached.createNode();
    detached.nodeMeta(first).set("name", "first");
    detached.nodeMeta(second).setContainer("text", new LoroText()).insert(
      0,
      "nested",
    );
    detached.createEdge(first, second);
    detached.createEdge(second, first);
    expect(detached.isAttached()).toBe(false);
    expect(detached.getAttached()).toBeUndefined();
    errorCode(() => detached.snapshot(), "DetachedContainer");
    const attached = doc.getMap("map").setContainer("graph", detached);
    expect(attached.kind()).toBe("Graph");
    expect(attached.isAttached()).toBe(true);
    expect(detached.getAttached()?.id).toBe(attached.id);
    expect(attached.parent()?.id).toBe(doc.getMap("map").id);
    expect(attached.nodes()).not.toEqual(detached.nodes());
    expect(attached.nodeCount()).toBe(2);
    expect(attached.edgeCount()).toBe(2);
    expect(attached.toJSON().nodes.map((node) => node.meta)).toEqual([{
      name: "first",
    }, { text: "nested" }]);
    expect(graph.parent()).toBeUndefined();
    expect(doc.getContainerById(attached.id)?.kind()).toBe("Graph");
    expect(doc.getGraph(attached.id).id).toBe(attached.id);
    expect(getType(doc.getMap("map").get("graph"))).toBe("Graph");
    expect(doc.getList("list").insertContainer(0, new LoroGraph()).kind()).toBe(
      "Graph",
    );
    expect(
      doc.getMovableList("movable").insertContainer(0, new LoroGraph()).kind(),
    ).toBe("Graph");
    expect(isContainer(attached)).toBe(true);
    expect(isContainerId(newRootContainerID("graph", "Graph"))).toBe(true);
    expect(
      isContainerId(newContainerID({ peer: MAX_PEER, counter: 0 }, "Graph")),
    ).toBe(true);
    expect(() => doc.getGraph(doc.getMap("map").id)).toThrow();
    expectTypeOf(attached).toMatchTypeOf<Container>();
    expectTypeOf(getType(attached)).toEqualTypeOf<"Graph">();
    expectTypeOf(first).not.toMatchTypeOf<GraphEdgeId>();
  });

  it("recursively resolves typed metadata without following topology cycles or committing", () => {
    const { doc, graph } = setup();
    const node = graph.createNode();
    const edge = graph.createEdge(node, node);
    const meta = graph.nodeMeta(node);
    meta.set("raw", { nested: [1, true] });
    meta.setContainer("text", new LoroText()).insert(0, "hello");
    graph.edgeMeta(edge).setContainer("text", new LoroText()).insert(0, "loop");
    const pending = doc.getUncommittedOpsAsJson();
    const tree = graph.toContainerTree({ text: "delta" });
    expect(tree.type).toBe("Graph");
    expect(tree.value.nodes[0].meta).toMatchObject({
      type: "Map",
      cid: meta.id,
    });
    expect(tree.value.nodes[0].meta.value.raw).toEqual({
      type: "Value",
      value: { nested: [1, true] },
    });
    expect(tree.value.nodes[0].meta.value.text).toMatchObject({
      type: "Text",
      value: [{ insert: "hello" }],
    });
    expect(tree.value.edges[0].meta.value.text).toMatchObject({
      type: "Text",
      value: [{ insert: "loop" }],
    });
    expect(doc.toContainerTree({ roots: ["graph"], text: "delta" }).graph)
      .toEqual(tree);
    expect(graph.toJSON().nodes[0].meta.text).toBe("hello");
    expect(graph.getShallowValue().nodes[0].meta).toBe(meta.id);
    expect(doc.getUncommittedOpsAsJson()).toEqual(pending);
    errorCode(() => graph.snapshot(), "UncommittedChanges");
  });
});

describe("graph wire, history and events", () => {
  it("round-trips binary and JSON updates with 64-bit identity and restore tags", () => {
    const { doc, graph } = setup(MAX_PEER);
    const a = graph.createNode();
    const b = graph.createNode();
    const edge = graph.createEdge(a, b);
    graph.createEdge(b, a);
    graph.nodeMeta(a).setContainer("text", new LoroText()).insert(
      0,
      "metadata",
    );
    graph.deleteNode(a);
    graph.restoreNode(a);
    graph.deleteEdge(edge);
    graph.restoreEdge(edge);
    expect(a).toBe(`0@${MAX_PEER}`);
    expect(doc.getUncommittedOpsAsJson()).toBeDefined();
    for (const mode of ["update", "snapshot"] as const) {
      const other = new LoroDoc();
      const bytes = doc.export({ mode });
      other.import(bytes);
      other.import(bytes);
      expect(other.getGraph("graph").toJSON()).toEqual(graph.toJSON());
    }
    for (const compressed of [false, true]) {
      const json = doc.exportJsonUpdates(undefined, undefined, compressed);
      const graphOps = json.changes.flatMap((change) => change.ops).filter((
        op,
      ) => op.container.endsWith(":Graph"));
      expect(graphOps.length).toBeGreaterThan(0);
      for (const { content } of graphOps) {
        const [operation, fields] = Object.entries(content as GraphJsonOp)[0];
        expect(operation).toMatch(/^(create|delete|restore)_(node|edge)$/);
        expect(typeof fields.id).toBe("string");
        expect(fields.id).toMatch(`@${MAX_PEER}`);
        if ("source" in fields) {
          expect(fields.source).toMatch(`@${MAX_PEER}`);
          expect(fields.target).toMatch(`@${MAX_PEER}`);
        }
        if ("deletes" in fields) {
          expect(fields.deletes).toHaveLength(1);
          for (const tag of fields.deletes) expect(tag).toMatch(`@${MAX_PEER}`);
        }
      }
      const other = new LoroDoc();
      other.importJsonUpdates(JSON.parse(JSON.stringify(json)));
      expect(other.getGraph("graph").toJSON()).toEqual(graph.toJSON());
      const fromString = new LoroDoc();
      fromString.importJsonUpdates(JSON.stringify(json));
      expect(fromString.getGraph("graph").toJSON()).toEqual(graph.toJSON());
    }
    const span = doc.exportJsonInIdSpan({
      peer: MAX_PEER,
      counter: 0,
      length: 4,
    });
    expect(span.flatMap((change) => change.ops)).toHaveLength(4);
  });

  it("preserves cycles created concurrently and merges nested Text metadata", () => {
    const { doc: left, graph: l } = setup("1");
    const a = l.createNode();
    const b = l.createNode();
    l.nodeMeta(a).setContainer("text", new LoroText());
    left.commit();
    const right = left.fork();
    right.setPeerId("2");
    const r = right.getGraph("graph");
    const ab = l.createEdge(a, b);
    const ba = r.createEdge(b, a);
    (l.nodeMeta(a).get("text") as LoroText).insert(0, "left");
    (r.nodeMeta(a).get("text") as LoroText).insert(0, "right");
    const leftBytes = left.export({ mode: "update" });
    const rightBytes = right.export({ mode: "update" });
    left.import(rightBytes);
    right.import(leftBytes);
    expect(l.toJSON()).toEqual(r.toJSON());
    expect(new Set(l.edges())).toEqual(new Set([ab, ba]));
    expect(l.snapshot().analyzeCycles().isAcyclic).toBe(false);
    expect((l.nodeMeta(a).get("text") as LoroText).length).toBe(9);
  });

  it("reconstructs derived edge visibility from events across checkout, delete and restore", async () => {
    const { doc, graph } = setup(MAX_PEER);
    const error = vi.spyOn(console, "error").mockImplementation(() => {});
    const nodes = new Map<GraphNodeId, GraphNodeRecord>();
    const edges = new Map<GraphEdgeId, GraphEdgeRecord>();
    const observed: GraphDiff[] = [];
    const off = graph.subscribe((event) => {
      for (const entry of event.events) {
        if (entry.diff.type !== "graph") continue;
        observed.push(entry.diff);
        for (const [id, value] of Object.entries(entry.diff.diff.nodes)) {
          if (value === null) nodes.delete(id as GraphNodeId);
          else nodes.set(id as GraphNodeId, value);
        }
        for (const [id, value] of Object.entries(entry.diff.diff.edges)) {
          if (value === null) edges.delete(id as GraphEdgeId);
          else edges.set(id as GraphEdgeId, value);
        }
      }
    });
    try {
      const a = graph.createNode();
      const b = graph.createNode();
      const edge = graph.createEdge(a, b);
      doc.commit();
      const before = doc.frontiers();
      graph.deleteNode(a);
      doc.commit();
      expect(observed.at(-1)?.diff.edges[edge]?.visible).toBe(false);
      expect(observed.at(-1)?.diff.nodes[a]?.deleteTags[0]).toMatch(
        `@${MAX_PEER}`,
      );
      expect([...nodes.values()]).toEqual(graph.nodeRecords());
      expect([...edges.values()]).toEqual(graph.edgeRecords());
      doc.checkout(before);
      expect([...nodes.values()]).toEqual(graph.nodeRecords());
      expect([...edges.values()]).toEqual(graph.edgeRecords());
      doc.checkoutToLatest();
      graph.restoreNode(a);
      doc.commit();
      expect(observed.at(-1)?.diff.edges[edge]?.visible).toBe(true);
      expect([...edges.values()]).toEqual(graph.edgeRecords());
      doc.checkout([]);
      expect(nodes.size).toBe(0);
      expect(edges.size).toBe(0);
      await new Promise((resolve) => setTimeout(resolve, 0));
      expect(error.mock.calls.flat().join(" ")).not.toContain(
        "[LORO_INTERNAL_ERROR] Event not called",
      );
    } finally {
      off();
      error.mockRestore();
    }
  });

  it("converts Graph diffs back to native operations and keeps selective undo", () => {
    const { doc, graph } = setup();
    const a = graph.createNode();
    const b = graph.createNode();
    const edge = graph.createEdge(a, b);
    doc.commit();
    const before = doc.frontiers();
    const other = doc.fork();
    graph.deleteNode(a);
    doc.commit();
    const diff = doc.diff(before, doc.frontiers(), true);
    other.applyDiff(JSON.parse(JSON.stringify(diff)));
    expect(other.getGraph("graph").getEdge(edge)).toBeUndefined();
    expect(other.getGraph("graph").edgeRecord(edge)?.alive).toBe(true);

    const undo = new UndoManager(doc, { mergeInterval: 0 });
    graph.restoreNode(a);
    doc.commit();
    graph.deleteEdge(edge);
    doc.commit();
    undo.undo();
    expect(graph.getEdge(edge)).toBeDefined();
    undo.redo();
    expect(graph.getEdge(edge)).toBeUndefined();
  });
});

describe("opt-in graph helpers", () => {
  it("keeps snapshot, analysis and planning pure, and refuses pending edits", () => {
    const { doc, graph } = setup();
    const node = graph.createNode();
    graph.createEdge(node, node);
    const listener = vi.fn();
    doc.subscribe(listener);
    const pending = doc.getUncommittedOpsAsJson();
    errorCode(() => graph.snapshot(), "UncommittedChanges");
    expect(doc.getUncommittedOpsAsJson()).toEqual(pending);
    expect(listener).not.toHaveBeenCalled();
    doc.commit();
    const version = doc.version().toJSON();
    const frontiers = doc.frontiers();
    listener.mockClear();
    const snapshot = graph.snapshot();
    const json = snapshot.toJSON();
    json.nodes.length = 0;
    json.edges.length = 0;
    expect(snapshot.nodes).toEqual([node]);
    expect(snapshot.analyzeCycles().isAcyclic).toBe(false);
    expect(snapshot.planBreakCycles().isEmpty()).toBe(false);
    expect(doc.version().toJSON()).toEqual(version);
    expect(doc.frontiers()).toEqual(frontiers);
    expect(listener).not.toHaveBeenCalled();
    expect(graph.edgeCount()).toBe(1);
  });

  it("applies only the selected plan, rejects JSON forgeries, and flushes event queues", async () => {
    const { doc, graph } = setup();
    const a = graph.createNode();
    const b = graph.createNode();
    const ab = graph.createEdge(a, b);
    const ba = graph.createEdge(b, a);
    const excluded = graph.createEdge(a, a);
    doc.commit();
    const snapshot = graph.snapshot();
    const plan = snapshot.planBreakCycles([ba, ab, ba]);
    const json = plan.toJSON();
    expect(json.scope).toEqual({ type: "selected", edges: [ab, ba] });
    expect(json.deletions.map((item) => item.edge)).toEqual([ba]);
    json.deletions.push({ edge: excluded, reason: { type: "selfLoop" } });
    expect(() => graph.applyRepair(json as unknown as GraphRepairPlan))
      .toThrow();
    errorCode(() => doc.getGraph("other").applyRepair(plan), "WrongGraph");
    const events = vi.fn();
    graph.subscribe(events);
    const error = vi.spyOn(console, "error").mockImplementation(() => {});
    try {
      expect(graph.applyRepair(plan)).toBe(1);
      expect(events).not.toHaveBeenCalled();
      errorCode(() => graph.applyRepair(plan), "UncommittedChanges");
      doc.commit({ origin: "explicit-graph-repair" });
      expect(events).toHaveBeenCalledTimes(1);
      errorCode(() => graph.applyRepair(plan), "StalePlan");
      expect(graph.edges()).toEqual([ab, excluded]);
      expect(graph.snapshot().analyzeCycles([ab]).isAcyclic).toBe(true);
      expect(graph.snapshot().analyzeCycles().isAcyclic).toBe(false);
      await new Promise((resolve) => setTimeout(resolve, 0));
      expect(error.mock.calls.flat().join(" ")).not.toContain(
        "[LORO_INTERNAL_ERROR] Event not called",
      );
    } finally {
      error.mockRestore();
    }
  });

  it("rejects stale plans and invalid selections; empty plans write no operations", () => {
    const { doc, graph } = setup();
    const node = graph.createNode();
    const loop = graph.createEdge(node, node);
    doc.commit();
    const snapshot = graph.snapshot();
    const empty = snapshot.planBreakCycles([]);
    const version = doc.version().toJSON();
    expect(graph.applyRepair(empty)).toBe(0);
    expect(graph.applyRepair(empty)).toBe(0);
    expect(doc.getUncommittedOpsAsJson()).toBeUndefined();
    expect(doc.version().toJSON()).toEqual(version);
    errorCode(
      () => snapshot.analyzeCycles(["0@0" as GraphEdgeId]),
      "InvalidSelection",
    );
    expect(() => snapshot.planBreakCycles(undefined, "unknown" as any))
      .toThrow();
    expect(() => snapshot.analyzeCycles([1] as any)).toThrow();
    const plan = snapshot.planBreakCycles([loop]);
    doc.getMap("other").set("changed", true);
    errorCode(() => graph.applyRepair(plan), "UncommittedChanges");
    doc.commit();
    errorCode(() => graph.applyRepair(plan), "StalePlan");
    expect(graph.getEdge(loop)).toBeDefined();
    doc.checkout(snapshot.version);
    expect(graph.snapshot().version).toEqual(snapshot.version);
    expect(doc.oplogFrontiers()).not.toEqual(graph.snapshot().version);
    errorCode(() => graph.applyRepair(plan), "LoroError");
    doc.checkoutToLatest();
    expect(graph.applyRepair(graph.snapshot().planBreakCycles())).toBe(1);
    doc.commit();
    graph.restoreEdge(loop);
    doc.commit();
    expect(graph.snapshot().analyzeCycles().isAcyclic).toBe(false);
  });
});
