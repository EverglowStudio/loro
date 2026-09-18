---
---

Add the Rust `loro::LoroDoc::import_updates_batch(&[&[u8]])` API for current
FastUpdates blobs. It skips metadata decoding/sorting and shares the existing
batch execution guard and single final state checkout. The ordinary decoder
still validates checksums and bodies; binary encoding and CRDT algorithms are
unchanged. Snapshot and shallow-snapshot inputs are rejected by header preflight.

The returned pending ranges describe all imported operations still absent from
the oplog, including earlier calls and empty/single-item batches. Applied overlap
prefixes are excluded before per-peer bounding ranges are merged. These ranges
are neither exact missing dependencies nor pending-blob counts. Existing
`import_batch` behavior is preserved, including mixed snapshots and its zero/single
item shortcuts.

Borrowed inputs remove the requirement for caller-owned Vec copies; decoding
still copies and allocates. This is not zero-copy, nor an ACID import: decode
errors may leave successful updates applied or pending. Final-checkout failure on
an attached document retains the existing batch rollback and cleanup behavior.
No allocation reduction or measured performance improvement is claimed here.

The native `import_updates_batch_perf` example compares both APIs in the same
binary with fixed fixtures, per-sample JSON output and full correctness checks.
This changeset has no npm release entry because this adds only a Rust API, with
no WASM/JavaScript binding change.

Pin the direct generic-btree dependencies of loro, loro-internal and loro-delta
to the existing in-tree path while retaining the version requirement. Git
consumers do not inherit this workspace's root patch, so explicit paths keep
consumer source provenance aligned with standalone validation. Compared with
registry 0.10.7, the existing in-tree source has small Clippy/lifetime cleanups;
this source correction does not introduce a new B-tree algorithm or claim speedup.
