import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "vitest";

import {
  ContainerType,
  LoroUnsupportedGraphError,
  containerTypeFromHistoricalByte,
  containerTypeFromRawByte,
  decodeChangeBlock,
  decodeChangeValue,
  decodeContainerArena,
  decodeContainerId,
  decodeContainerStateSnapshot,
  decodeContainerStateWrapper,
  decodeEncodedChangeBlock,
  decodeFastSnapshot,
  decodeFastUpdates,
  decodeLazyStateSnapshotStore,
  decodePostcardContainerId,
  decodePostcardOptionalContainerId,
  decodePostcardValue,
  decodeStateSnapshotStore,
  encodeChangeBlock,
  encodeChangeBlockKey,
  encodeChangeValue,
  encodeContainerArena,
  encodeContainerId,
  encodeContainerStateWrapper,
  encodeEncodedChangeBlock,
  encodeFastSnapshot,
  encodeFastUpdates,
  encodePostcardContainerId,
  encodePostcardFrontiers,
  encodePostcardOptionalContainerId,
  encodePostcardValue,
  encodePostcardVersionVector,
  encodeSstable,
  encodeStateSnapshotStore,
  unknownContainerType,
  type ChangeLoroValue,
  type ContainerId,
  type EncodedLoroValue,
  type FastSnapshotBody,
} from "../src/codec/index";
import { Cursor, LoroDoc, decodeImportBlobMeta, isContainer, redactJsonUpdates } from "../src/index";
import { mergeableMarker, parseMergeableMarker } from "../src/runtime/mergeable";
import type { JsonSchema, LoroEventBatch } from "../src/runtime/types";

const graphType = unknownContainerType(6);
const mapId: ContainerId = {
  kind: "root", name: "map", containerType: ContainerType.Map,
};
const graphId: ContainerId = { kind: "root", name: "graph", containerType: graphType };
const normalGraphId: ContainerId = {
  kind: "normal", peer: 1n, counter: 0, containerType: graphType,
};

function rejectsGraph(action: () => unknown): void {
  assert.throws(action, (error: unknown) => {
    assert.ok(error instanceof LoroUnsupportedGraphError);
    assert.equal(error.code, "UNSUPPORTED_GRAPH");
    assert.match(error.message, /unsupported Graph/);
    return true;
  });
}

function mapBlock(
  value: ChangeLoroValue = { type: "i64", value: 1n },
  peer = 1n,
): Uint8Array {
  return encodeChangeBlock({
    peers: [peer], keys: ["map", "key"], containers: [mapId], positions: [],
    changes: [{
      id: { peer, counter: 0 }, timestamp: 0n, dependencies: [], lamport: 0,
      message: undefined,
      operations: [{
        container: mapId, counter: 0, length: 1,
        content: { type: "map-insert", key: "key", value },
      }],
    }],
  });
}

// This is a type-boundary fixture, not an implementation of Graph operation encoding.
function graphBlock(): Uint8Array {
  const block = decodeEncodedChangeBlock(mapBlock());
  return encodeEncodedChangeBlock({
    ...block,
    containerIds: encodeContainerArena([graphId], [1n], ["map", "key", "graph"]),
    values: new Uint8Array(), // Must reject the arena before reading an unknown payload.
  });
}

function graphState(id = graphId): Uint8Array {
  return encodeStateSnapshotStore({
    kind: "sstable", frontiers: undefined,
    containers: [{
      id,
      wrapper: {
        containerType: graphType, depth: 1n, parent: undefined,
        state: { kind: graphType, payload: Uint8Array.of(0xff) },
      },
    }],
  });
}

function emptySnapshot(): FastSnapshotBody {
  return decodeFastSnapshot(new LoroDoc().export({ mode: "snapshot" }));
}

function assertImportUnchanged(bytes: Uint8Array, batch = false): void {
  const doc = new LoroDoc();
  doc.setPeerId(42n);
  doc.getMap("local").set("keep", true);
  doc.commit();
  const before = doc.toJSON();
  const version = doc.oplogVersion().encode();
  const remote = encodeFastUpdates([mapBlock()]);
  rejectsGraph(() => batch ? doc.importBatch([remote, bytes]) : doc.import(bytes));
  assert.deepEqual(doc.toJSON(), before);
  assert.deepEqual(doc.oplogVersion().encode(), version);
}

test("rejects raw and historical Graph IDs, including wrapper parents and values", () => {
  rejectsGraph(() => containerTypeFromRawByte(6));
  rejectsGraph(() => containerTypeFromHistoricalByte(6));
  for (const id of [graphId, normalGraphId]) {
    rejectsGraph(() => decodeContainerId(encodeContainerId(id)));
    rejectsGraph(() => decodePostcardContainerId(encodePostcardContainerId(id)));
    rejectsGraph(() => decodePostcardOptionalContainerId(
      encodePostcardOptionalContainerId(id),
    ));
    const cursor = Uint8Array.of(0, ...encodePostcardContainerId(id), 0, 0);
    rejectsGraph(() => Cursor.decode(cursor));
    const nested: EncodedLoroValue = {
      type: "map", value: [["child", { type: "list", value: [{ type: "container", value: id }] }]],
    };
    rejectsGraph(() => decodePostcardValue(encodePostcardValue(nested)));
    const wrapper = encodeContainerStateWrapper({
      containerType: ContainerType.Counter, depth: 2n, parent: id,
      state: { kind: ContainerType.Counter, bits: 0n },
    });
    rejectsGraph(() => decodeContainerStateWrapper(wrapper));
  }
});

test("rejects Graph arenas and nested value tags before integrating any update", () => {
  const arena = encodeContainerArena([graphId], [], ["graph"]);
  rejectsGraph(() => decodeContainerArena(arena, [], ["graph"]));
  const value: ChangeLoroValue = {
    type: "map", value: [[0n, { type: "list", value: [{ type: "container-type", value: 6 }] }]],
  };
  rejectsGraph(() => decodeChangeValue(encodeChangeValue({ type: "loro-value", value })));
  for (const block of [graphBlock(), mapBlock(value)]) {
    rejectsGraph(() => decodeChangeBlock(block));
    const update = encodeFastUpdates([mapBlock(undefined, 2n), block]);
    rejectsGraph(() => decodeImportBlobMeta(update));
    assertImportUnchanged(update);
    assertImportUnchanged(update, true);
  }
});

test("rejects Graph state in eager and lazy readers, even for unreferenced children", () => {
  rejectsGraph(() => decodeContainerStateSnapshot(graphType, new Uint8Array()));
  for (const id of [graphId, normalGraphId]) {
    const state = graphState(id);
    rejectsGraph(() => decodeStateSnapshotStore(state));
    rejectsGraph(() => decodeLazyStateSnapshotStore(state));
    const snapshot = encodeFastSnapshot({ ...emptySnapshot(), state });
    rejectsGraph(() => LoroDoc.fromSnapshot(snapshot));
    rejectsGraph(() => decodeImportBlobMeta(snapshot));
    assertImportUnchanged(snapshot);
    assertImportUnchanged(snapshot, true);
  }
});

test("checks state wrappers and Graph references even when the state key is an old type", () => {
  const wrapper = encodeContainerStateWrapper({
    containerType: graphType, depth: 1n, parent: undefined,
    state: { kind: graphType, payload: Uint8Array.of(0xff) },
  });
  const state = encodeSstable([{ key: encodeContainerId(mapId), value: wrapper }]);
  rejectsGraph(() => decodeLazyStateSnapshotStore(state));
  const nested = encodeStateSnapshotStore({
    kind: "sstable", frontiers: undefined,
    containers: [{
      id: mapId,
      wrapper: {
        containerType: ContainerType.Map, depth: 1n, parent: undefined,
        state: {
          kind: ContainerType.Map,
          values: [["child", { type: "container", value: normalGraphId }]],
          deletedKeys: [], peers: [1n],
          metadata: [{ key: "child", peerIndex: 0n, lamport: 0n }],
        },
      },
    }],
  });
  rejectsGraph(() => decodeStateSnapshotStore(nested));
  rejectsGraph(() => decodeLazyStateSnapshotStore(nested));
});

test("checks shallow roots even when importing into an existing document", () => {
  const snapshot = encodeFastSnapshot({
    ...emptySnapshot(), shallowRootState: graphState(),
  });
  rejectsGraph(() => LoroDoc.fromSnapshot(snapshot));
  rejectsGraph(() => decodeImportBlobMeta(snapshot));
  assertImportUnchanged(snapshot);
  assertImportUnchanged(snapshot, true);
});

test("rejects a valid Graph mergeable marker before changing imported state", () => {
  const value = mergeableMarker(mapId, "key", graphType);
  assertImportUnchanged(encodeFastUpdates([mapBlock({ type: "binary", value })]));
  // Produce state bytes directly: setting this marker through the runtime is unsupported.
  const state = encodeStateSnapshotStore({
    kind: "sstable", frontiers: undefined,
    containers: [{ id: mapId, wrapper: {
      containerType: ContainerType.Map, depth: 1n, parent: undefined,
      state: {
        kind: ContainerType.Map, values: [["key", { type: "binary", value }]],
        deletedKeys: [], peers: [1n], metadata: [{ key: "key", peerIndex: 0n, lamport: 0n }],
      },
    } }],
  });
  const snapshot = encodeFastSnapshot({ ...emptySnapshot(), state });
  rejectsGraph(() => LoroDoc.fromSnapshot(snapshot));
  rejectsGraph(() => decodeImportBlobMeta(snapshot));
  assertImportUnchanged(snapshot, true);
});

test("checks every retained history block before installing a lazy current snapshot", () => {
  const current = new LoroDoc();
  current.setPeerId(2n);
  current.getMap("map").set("key", 1);
  const base = decodeFastSnapshot(current.export({ mode: "snapshot" }));
  for (const block of [graphBlock(), mapBlock({ type: "container-type", value: 6 })]) {
    const snapshot = encodeFastSnapshot({
      ...base,
      oplog: encodeSstable([
        { key: encodeChangeBlockKey({ peer: 1n, counter: 0 }), value: block },
        { key: encodeChangeBlockKey({ peer: 2n, counter: 0 }), value: mapBlock(undefined, 2n) },
        {
          key: new TextEncoder().encode("fr"),
          value: encodePostcardFrontiers([{ peer: 2n, counter: 0 }]),
        },
        {
          key: new TextEncoder().encode("vv"),
          value: encodePostcardVersionVector([
            { peer: 1n, counter: 1 }, { peer: 2n, counter: 1 },
          ]),
        },
      ]),
    });
    rejectsGraph(() => LoroDoc.fromSnapshot(snapshot));
    rejectsGraph(() => decodeImportBlobMeta(snapshot));
    assertImportUnchanged(snapshot, true);
  }
});

function schema(container: string, value: unknown): JsonSchema {
  return {
    schema_version: 1, peers: null, start_version: {},
    changes: [{
      id: "0@1", timestamp: 0, lamport: 0, deps: [], msg: null,
      ops: [{ container, counter: 0, content: { type: "insert", key: "child", value } }],
    }],
  } as JsonSchema;
}

test("rejects root, normal and peer-compressed Graph JSON, including nested references", () => {
  for (const input of [
    schema("cid:root-graph:Graph", null),
    schema("cid:0@1:Graph", null),
    { ...schema("cid:0@0:Graph", null), peers: ["1"] },
    schema("cid:root-map:Map", { child: ["🦜:cid:0@1:Graph"] }),
  ]) {
    const doc = new LoroDoc();
    rejectsGraph(() => doc.importJsonUpdates(input));
    rejectsGraph(() => doc.importJsonUpdates(JSON.stringify(input)));
    rejectsGraph(() => redactJsonUpdates(input, {}));
    assert.deepEqual(doc.toJSON(), {});
    assert.equal(doc.opCount(), 0);
  }
});

test("rejects Graph references in serialized map and list diffs before applying a batch", () => {
  const batches = [
    [["cid:root-graph:Graph", { type: "map", updated: {} }]],
    [["cid:root-map:Map", {
      type: "map", updated: { keep: 1, graph: "🦜:cid:0@1:Graph" },
    }]],
    [["cid:root-list:List", {
      type: "list", diff: [{ insert: [1, "🦜:cid:0@1:Graph"] }],
    }]],
  ];
  for (const batch of batches) {
    const doc = new LoroDoc();
    const parsed = JSON.parse(JSON.stringify(batch)) as Parameters<LoroDoc["applyDiff"]>[0];
    rejectsGraph(() => doc.applyDiff(parsed));
    assert.deepEqual(doc.toJSON(), {});
    assert.equal(doc.opCount(), 0);
  }
});

for (const initialState of ["empty", "committed", "dirty"] as const) {
  test(`rejects Graph map diff markers without changing document state (${initialState})`, () => {
    const doc = new LoroDoc();
    doc.setPeerId(7n);
    if (initialState !== "empty") {
      doc.getMap("map").set("saved", 1);
      doc.commit();
    }
    const events: LoroEventBatch[] = [];
    const updates: Uint8Array[] = [];
    doc.subscribe((event) => events.push(event));
    doc.subscribeLocalUpdates((bytes) => updates.push(bytes));
    if (initialState === "dirty") doc.getMap("map").set("draft", 2);
    // These reads must not commit or otherwise clear the pending transaction.
    const state = () => ({
      value: doc.toJSON(),
      pendingLength: doc.getPendingTxnLength(),
      pending: doc.getUncommittedOpsAsJson(),
      version: doc.version().toJSON(),
      oplogVersion: doc.oplogVersion().toJSON(),
      frontiers: doc.frontiers(),
      oplogFrontiers: doc.oplogFrontiers(),
      opCount: doc.opCount(),
      changeCount: doc.changeCount(),
    });
    const before = state();
    const marker = mergeableMarker(mapId, "key", graphType);
    const batches: Parameters<LoroDoc["applyDiff"]>[0][] = [
      [["cid:root-map:Map", { type: "map", updated: { key: marker } }]],
      [["cid:root-map:Map", { type: "map", updated: { keep: 1, key: marker } }]],
      [
        ["cid:root-earlier:Map", { type: "map", updated: { keep: 1 } }],
        ["cid:root-map:Map", { type: "map", updated: { key: marker } }],
      ],
    ];
    for (const batch of batches) {
      rejectsGraph(() => doc.applyDiff(batch));
      assert.deepEqual(state(), before);
      assert.deepEqual(events, []);
      assert.deepEqual(updates, []);
    }

    doc.getMap("map").set("after", 3);
    assert.equal(doc.getPendingTxnLength(), before.pendingLength + 1);
    doc.commit();
    const expected = {
      map: {
        ...(initialState !== "empty" ? { saved: 1 } : {}),
        ...(initialState === "dirty" ? { draft: 2 } : {}),
        after: 3,
      },
    };
    assert.deepEqual(doc.toJSON(), expected);
    assert.equal(doc.getPendingTxnLength(), 0);
    assert.equal(doc.getUncommittedOpsAsJson(), undefined);
    const expectedVersion = new Map([["7", (before.version.get("7") ?? 0) + 1]]);
    assert.deepEqual(doc.version().toJSON(), expectedVersion);
    assert.deepEqual(doc.oplogVersion().toJSON(), expectedVersion);
    assert.equal(doc.opCount(), before.opCount + before.pendingLength + 1);
    assert.equal(events.length, 1);
    assert.deepEqual(events[0]!.events, [{
      target: "cid:root-map:Map", path: ["map"],
      diff: { type: "map", updated: {
        ...(initialState === "dirty" ? { draft: 2 } : {}), after: 3,
      } },
    }]);
    assert.equal(updates.length, 1);
    // A fresh reader must accept the resulting history without a hidden Graph op.
    const restored = new LoroDoc();
    restored.import(doc.export({ mode: "update" }));
    assert.deepEqual(restored.toJSON(), expected);
    assert.deepEqual(restored.oplogVersion().toJSON(), expectedVersion);
  });
}

test("map diff preflight preserves ordinary binary values and old mergeable markers", () => {
  const badCrc = mergeableMarker(mapId, "bad-crc", graphType);
  badCrc[7] = badCrc[7]! ^ 1;
  const binaryValues = {
    "bad-crc": badCrc,
    "wrong-key": mergeableMarker(mapId, "different-key", graphType),
    "wrong-parent": mergeableMarker({ ...mapId, name: "other" }, "wrong-parent", graphType),
    unknown: mergeableMarker(mapId, "unknown", unknownContainerType(19)),
    ordinary: Uint8Array.of(6, 0x86, 9, 6),
  };
  const doc = new LoroDoc();
  const listBinary = mergeableMarker(mapId, "key", graphType);
  doc.applyDiff([
    ["cid:root-map:Map", { type: "map", updated: binaryValues }],
    ["cid:root-list:List", { type: "list", diff: [{ insert: [listBinary] }] }],
  ]);
  for (const [key, value] of Object.entries(binaryValues)) {
    assert.deepEqual(doc.getMap("map").get(key), value);
  }
  assert.deepEqual(doc.getList("list").get(0), listBinary);
  for (const [type, kind] of [
    [ContainerType.Map, "Map"], [ContainerType.List, "List"],
    [ContainerType.Text, "Text"], [ContainerType.Tree, "Tree"],
    [ContainerType.MovableList, "MovableList"], [ContainerType.Counter, "Counter"],
  ] as const) {
    const key = `old-${type}`;
    doc.applyDiff([["cid:root-map:Map", {
      type: "map", updated: { [key]: mergeableMarker(mapId, key, type) },
    }]]);
    const child = doc.getMap("map").get(key);
    assert.ok(isContainer(child));
    assert.equal(child.kind(), kind);
  }
});

test("keeps old type mappings, other unknown types, and ordinary Graph-like data", () => {
  const raw = [ContainerType.Map, ContainerType.List, ContainerType.Text,
    ContainerType.Tree, ContainerType.MovableList, ContainerType.Counter];
  const historical = [ContainerType.Text, ContainerType.Map, ContainerType.List,
    ContainerType.MovableList, ContainerType.Tree, ContainerType.Counter];
  for (let i = 0; i < 6; i++) {
    assert.equal(containerTypeFromRawByte(i), raw[i]);
    assert.equal(containerTypeFromHistoricalByte(i), historical[i]);
  }
  const unknown = unknownContainerType(19);
  assert.deepEqual(containerTypeFromRawByte(19), unknown);
  assert.deepEqual(containerTypeFromHistoricalByte(19), unknown);
  assert.deepEqual(decodeContainerStateSnapshot(unknown, Uint8Array.of(6)), {
    kind: unknown, payload: Uint8Array.of(6),
  });
  const binary = { type: "binary" as const, value: Uint8Array.of(6, 0x86, 9, 6) };
  assert.deepEqual(decodeChangeValue(encodeChangeValue(binary)), binary);
  const future = { type: "future" as const, tag: 0x91, data: Uint8Array.of(6) };
  assert.deepEqual(decodeChangeValue(encodeChangeValue(future)), future);
  const doc = new LoroDoc();
  doc.importJsonUpdates(schema("cid:root-map:Map", { text: "cid:0@1:Graph", type: "Graph" }));
  assert.deepEqual(doc.toJSON(), { map: { child: { text: "cid:0@1:Graph", type: "Graph" } } });
  const marker = mergeableMarker(mapId, "key", graphType);
  rejectsGraph(() => parseMergeableMarker(mapId, "key", marker));
  marker[7] = marker[7]! ^ 1;
  assert.equal(parseMergeableMarker(mapId, "key", marker), undefined);
});

test("still reads checked-in Rust updates and current snapshots without Graph", () => {
  const fixture = (name: string): Uint8Array => new Uint8Array(readFileSync(
    new URL(`./fixtures/rust/${name}`, import.meta.url),
  ));
  for (const block of decodeFastUpdates(fixture("updates.blob"))) {
    const decoded = decodeChangeBlock(block);
    assert.deepEqual(decodeChangeBlock(encodeChangeBlock(decoded)).changes, decoded.changes);
  }
  const snapshot = fixture("snapshot.blob");
  const updatesDoc = new LoroDoc();
  updatesDoc.import(fixture("updates.blob"));
  assert.deepEqual(LoroDoc.fromSnapshot(snapshot).toJSON(), updatesDoc.toJSON());
  const reframed = encodeFastSnapshot(decodeFastSnapshot(snapshot));
  assert.deepEqual(decodeStateSnapshotStore(decodeFastSnapshot(snapshot).state),
    decodeStateSnapshotStore(decodeFastSnapshot(reframed).state));
});
