import { decodeEncodedChangeBlock } from "../codec/change-block";
import type { DecodedChangeBlock } from "../codec/change-block-codec";
import {
  decodeChangeKeys,
  decodeChangesHeader,
  decodeContainerArena,
  decodeEncodedOperations,
} from "../codec/change-block-tables";
import { decodeChangeValueContent } from "../codec/change-value";
import { decodeContainerId } from "../codec/container-id";
import {
  decodeLazyStateSnapshotStore as decodeLazyStore,
  decodeStateSnapshotStore as decodeStore,
  decodeContainerStateWrapper,
  type StateSnapshotContainerEntry,
} from "../codec/state-snapshot";
import { ContainerType } from "../codec/types";
import { parseMergeableMarker } from "./mergeable";

/** Check deferred history without constructing changes or installing runtime state. */
export function assertSupportedChangeBlock(bytes: Uint8Array): void {
  const encoded = decodeEncodedChangeBlock(bytes);
  const header = decodeChangesHeader(encoded.header, {
    changeCount: encoded.changeCount,
    counterStart: encoded.counterStart,
    counterLength: encoded.counterLength,
    lamportStart: encoded.lamportStart,
    lamportLength: encoded.lamportLength,
  });
  const keys = decodeChangeKeys(encoded.keys);
  const containers = decodeContainerArena(encoded.containerIds, header.peers, keys);
  let remaining = encoded.values;
  for (const row of decodeEncodedOperations(encoded.operations)) {
    const [value, rest] = decodeChangeValueContent(row.valueType, remaining);
    remaining = rest;
    const container = containers[row.containerIndex];
    const key = keys[row.property];
    if (
      container?.containerType === ContainerType.Map && key !== undefined &&
      value.type === "loro-value" && value.value.type === "binary"
    ) {
      parseMergeableMarker(container, key, value.value.value);
    }
  }
}

export function assertSupportedDecodedChangeBlock(block: DecodedChangeBlock): void {
  for (const change of block.changes) {
    for (const { container, content } of change.operations) {
      if (content.type === "map-insert" && content.value.type === "binary") {
        parseMergeableMarker(container, content.key, content.value.value);
      }
    }
  }
}

function assertSupportedMapState({ id, wrapper }: StateSnapshotContainerEntry): void {
  if (wrapper.state.kind !== ContainerType.Map) return;
  for (const [key, value] of wrapper.state.values) {
    if (value.type === "binary") parseMergeableMarker(id, key, value.value);
  }
}

export function decodeStateSnapshotStore(bytes: Uint8Array) {
  const store = decodeStore(bytes);
  if (store.kind === "sstable") store.containers.forEach(assertSupportedMapState);
  return store;
}

export function decodeLazyStateSnapshotStore(bytes: Uint8Array) {
  const store = decodeLazyStore(bytes);
  if (store.kind === "sstable") {
    for (const entry of store.table.entries()) {
      // Map binary values can activate mergeable children without a child-state entry.
      if ((entry.key[0]! & 0x7f) !== 0) continue;
      assertSupportedMapState({
        id: decodeContainerId(entry.key),
        wrapper: decodeContainerStateWrapper(entry.value),
      });
    }
  }
  return store;
}
