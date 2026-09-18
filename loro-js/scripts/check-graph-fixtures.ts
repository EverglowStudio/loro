import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";

import {
  decodeChangeBlock,
  decodeFastSnapshot,
  decodeFastUpdates,
  decodeSstable,
  decodeStateSnapshotStore,
} from "../src/codec/index";
import {
  LoroDoc,
  LoroUnsupportedGraphError,
  decodeImportBlobMeta,
  redactJsonUpdates,
  type JsonSchema,
} from "../src/index";

function checkNativeGraphFixtures(directory: string): void {
  const results: { file: string; bytes: number; sha256: string; checks: string[] }[] = [];
  const read = (file: string): Uint8Array => {
    const bytes = new Uint8Array(readFileSync(resolve(directory, file)));
    assert.ok(bytes.length > 0, `${file}: fixture is empty`);
    results.push({
      file,
      bytes: bytes.length,
      sha256: createHash("sha256").update(bytes).digest("hex"),
      checks: [],
    });
    return bytes;
  };
  const rejectsGraph = (check: string, action: () => unknown): void => {
    assert.throws(action, (error: unknown) => {
      assert.ok(error instanceof LoroUnsupportedGraphError, `${check}: wrong error`);
      assert.equal(error.code, "UNSUPPORTED_GRAPH");
      return true;
    }, `${check}: accepted a Graph fixture`);
    results.at(-1)!.checks.push(check);
  };
  const existingDoc = (): LoroDoc => {
    const doc = new LoroDoc();
    doc.setPeerId(42n);
    doc.getMap("local").set("keep", true);
    doc.commit();
    return doc;
  };
  const rejectsWithoutChanges = (
    check: string,
    importInto: (doc: LoroDoc) => unknown,
  ): void => {
    const doc = existingDoc();
    const before = doc.toJSON();
    const version = doc.oplogVersion().encode();
    let events = 0;
    doc.subscribe(() => { events += 1; });
    rejectsGraph(check, () => importInto(doc));
    assert.deepEqual(doc.toJSON(), before, `${check}: changed local state`);
    assert.deepEqual(doc.oplogVersion().encode(), version, `${check}: changed version`);
    assert.equal(events, 0, `${check}: emitted an event`);
  };

  const legacy = existingDoc().export({ mode: "update" });
  for (const file of [
    "graph-updates.bin",
    "graph-snapshot.bin",
    "graph-shallow.bin",
    "graph-state-only.bin",
  ]) {
    const bytes = read(file);
    rejectsGraph("fresh import", () => new LoroDoc().import(bytes));
    rejectsGraph("blob metadata", () => decodeImportBlobMeta(bytes));
    rejectsWithoutChanges("existing import", (doc) => doc.import(bytes));
    rejectsWithoutChanges("mixed batch", (doc) => doc.importBatch([legacy, bytes]));
    if (file === "graph-updates.bin") {
      rejectsGraph("semantic update codec", () => {
        for (const block of decodeFastUpdates(bytes)) decodeChangeBlock(block);
      });
    } else {
      rejectsGraph("fromSnapshot", () => LoroDoc.fromSnapshot(bytes));
      rejectsGraph("semantic snapshot codec", () => {
        const snapshot = decodeFastSnapshot(bytes);
        decodeStateSnapshotStore(snapshot.state);
        decodeStateSnapshotStore(snapshot.shallowRootState);
        for (const entry of decodeSstable(snapshot.oplog)) {
          if (entry.key.length === 12) decodeChangeBlock(entry.value);
        }
      });
    }
  }

  const json = new TextDecoder().decode(read("graph-updates.json"));
  const schema = JSON.parse(json) as JsonSchema;
  rejectsWithoutChanges("JSON string", (doc) => doc.importJsonUpdates(json));
  rejectsWithoutChanges("JSON object", (doc) => doc.importJsonUpdates(schema));
  rejectsGraph("JSON redaction", () => redactJsonUpdates(schema, {}));
  process.stdout.write(`${JSON.stringify({ status: "passed", results }, null, 2)}\n`);
}

try {
  if (process.argv.length !== 3) {
    throw new Error("usage: bun --no-install scripts/check-graph-fixtures.ts <fixture-dir>");
  }
  checkNativeGraphFixtures(resolve(process.argv[2]!));
} catch (error) {
  process.stderr.write(`${error instanceof Error ? error.message : String(error)}\n`);
  process.exitCode = 1;
}
