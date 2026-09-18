import { describe, expect, it } from "vitest";
import {
  LoroDoc,
  LoroEvent,
  LoroGraph,
  LoroText,
  UndoManager,
} from "../bundler/index";

// Public JS regressions for LG02/native review P1-02, P1-04, P1-05 and P1-R1. These exercise
// serialized diffs and delivered events, without reconstructing state by queries
// inside subscription callbacks.
describe("graph lifecycle and metadata regressions", () => {
  it("keeps remote node and edge metadata when undoing combined delete/restore", () => {
    const source = new LoroDoc();
    source.setPeerId("1");
    const graph = source.getGraph("graph");
    const node = graph.createNode();
    const edge = graph.createEdge(node, node);
    const nodeMeta = graph.nodeMeta(node);
    const edgeMeta = graph.edgeMeta(edge);
    nodeMeta.set("title", "base");
    edgeMeta.set("title", "base");
    source.commit();

    const remote = new LoroDoc();
    remote.setPeerId("2");
    remote.import(source.export({ mode: "snapshot" }));
    const undo = new UndoManager(source, { mergeInterval: 0 });
    // The undo interval contains lifecycle operations only. Full metadata
    // revival events must not become edits of these unchanged properties.
    graph.deleteNode(node);
    graph.deleteEdge(edge);
    graph.restoreNode(node);
    graph.restoreEdge(edge);
    source.commit();

    const remoteGraph = remote.getGraph("graph");
    remoteGraph.nodeMeta(node).set("title", "remote");
    remoteGraph.edgeMeta(edge).set("title", "remote");
    remote.commit();
    source.import(remote.export({ mode: "update" }));
    const titles = () => ({
      node: nodeMeta.get("title"),
      edge: edgeMeta.get("title"),
    });
    expect(titles()).toEqual({ node: "remote", edge: "remote" });

    // A net-zero lifecycle interval may make undo a no-op; either way it must
    // preserve both remote values and keep the original topology visible.
    undo.undo();
    expect(titles()).toEqual({ node: "remote", edge: "remote" });
    expect(graph.nodes()).toEqual([node]);
    expect(graph.edges()).toEqual([edge]);
  });

  it("uses stable object ID strings for node, edge and nested Text event paths", () => {
    const doc = new LoroDoc();
    doc.setPeerId("18446744073709551614");
    const graph = doc.getGraph("graph");
    const node = graph.createNode();
    const edge = graph.createEdge(node, node);
    const nodeMeta = graph.nodeMeta(node);
    const edgeMeta = graph.edgeMeta(edge);
    const nodeText = nodeMeta.setContainer("body", new LoroText());
    const edgeText = edgeMeta.setContainer("body", new LoroText());
    nodeText.insert(0, "node");
    edgeText.insert(0, "edge");
    doc.commit();

    const expectedPaths = [
      [nodeMeta.id, ["graph", node]],
      [edgeMeta.id, ["graph", edge]],
      [nodeText.id, ["graph", node, "body"]],
      [edgeText.id, ["graph", edge, "body"]],
    ] as const;
    const events: LoroEvent[] = [];
    const off = doc.subscribe((batch) => events.push(...batch.events));
    try {
      for (const round of [0, 1]) {
        if (round === 1) {
          // A lower peer inserts a row before the existing node. Paths must
          // continue to identify the same objects rather than row positions.
          doc.setPeerId("1");
          const earlierNode = graph.createNode();
          doc.commit();
          expect(graph.nodes()[0]).toBe(earlierNode);
        }
        events.length = 0;
        nodeMeta.set("title", `node ${round}`);
        edgeMeta.set("title", `edge ${round}`);
        nodeText.insert(0, `${round}:`);
        edgeText.insert(0, `${round}:`);
        doc.commit();

        for (const [target, expected] of expectedPaths) {
          const path = events.find((event) => event.target === target)?.path;
          expect(path).toEqual(expected);
          expect(typeof path?.[1]).toBe("string");
          expect(path?.[1]).not.toBe(nodeMeta.id);
          expect(path?.[1]).not.toBe(edgeMeta.id);
        }
      }
    } finally {
      off();
    }
  });

  it("remaps deletion tags when applying a compound lifecycle diff forward", () => {
    const source = new LoroDoc();
    source.setPeerId("1");
    const graph = source.getGraph("graph");
    const node = graph.createNode();
    const edge = graph.createEdge(node, node);
    source.commit();
    const before = source.frontiers();
    const target = source.fork();
    target.setPeerId("2");

    graph.deleteNode(node);
    graph.restoreNode(node);
    graph.deleteEdge(edge);
    graph.restoreEdge(edge);
    source.commit();
    const diff = source.diff(before, source.frontiers(), true);
    expect(() => target.applyDiff(JSON.parse(JSON.stringify(diff)))).not
      .toThrow();
    expect(target.getGraph("graph").toJSON()).toEqual(graph.toJSON());
    expect(target.getGraph("graph").getNode(node)).toBeDefined();
    expect(target.getGraph("graph").getEdge(edge)).toBeDefined();
  });

  it("does not leave its own deletion behind when applying a compound diff backward", () => {
    const source = new LoroDoc();
    source.setPeerId("1");
    const graph = source.getGraph("graph");
    const node = graph.createNode();
    const edge = graph.createEdge(node, node);
    source.commit();
    const before = source.frontiers();
    const expected = graph.toJSON();
    graph.deleteNode(node);
    graph.restoreNode(node);
    graph.deleteEdge(edge);
    graph.restoreEdge(edge);
    source.commit();

    const target = source.fork();
    target.setPeerId("2");
    const diff = source.diff(source.frontiers(), before, true);
    expect(() => target.applyDiff(JSON.parse(JSON.stringify(diff)))).not
      .toThrow();
    expect(target.getGraph("graph").toJSON()).toEqual(expected);
    expect(target.getGraph("graph").nodeRecord(node)?.deleteTags).toEqual([]);
    expect(target.getGraph("graph").edgeRecord(edge)?.deleteTags).toEqual([]);
  });

  it("delivers unchanged node, edge and nested Text metadata when a hidden graph returns", () => {
    const doc = new LoroDoc();
    doc.setPeerId("1");
    const map = doc.getMap("map");
    const graph = map.setContainer("graph", new LoroGraph());
    const node = graph.createNode();
    const edge = graph.createEdge(node, node);
    const nodeMeta = graph.nodeMeta(node);
    const edgeMeta = graph.edgeMeta(edge);
    nodeMeta.set("title", "node title");
    edgeMeta.set("title", "edge title");
    const text = nodeMeta.setContainer("text", new LoroText());
    text.insert(0, "restored text");
    doc.commit();
    const visibleVersion = doc.frontiers();
    map.delete("graph");
    doc.commit();

    const events: LoroEvent[] = [];
    const off = doc.subscribe((batch) => events.push(...batch.events));
    try {
      doc.checkout(visibleVersion);
      expect(events.find((event) => event.target === graph.id)?.diff.type).toBe(
        "graph",
      );
      expect(events.find((event) => event.target === nodeMeta.id)?.diff)
        .toEqual({
          type: "map",
          updated: { title: "node title", text: expect.any(LoroText) },
        });
      expect(events.find((event) => event.target === edgeMeta.id)?.diff)
        .toEqual({
          type: "map",
          updated: { title: "edge title" },
        });
      expect(events.find((event) => event.target === text.id)?.diff).toEqual({
        type: "text",
        diff: [{ insert: "restored text" }],
      });
    } finally {
      off();
    }
  });
});
