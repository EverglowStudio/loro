---
"loro": minor
"loro-internal": minor
"loro-common": minor
"loro-crdt": minor
"loro.js": patch
---

Add the native LoroGraph directed property multigraph with stable node and edge identities, associated metadata maps, incremental adjacency and visibility indexes, remove-wins deletion and explicit restore, native history and snapshots, graph diffs, and selective undo. Graph uses container tag 6 and requires a reader that supports this fork's graph protocol. Existing container type tags remain unchanged.

Expose LoroGraph and explicit cycle-analysis/repair APIs in the WASM package with lossless string identities. The independent TypeScript and MoonBit readers reject Graph documents with an explicit unsupported-type error instead of silently dropping data.
