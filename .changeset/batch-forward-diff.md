---
---

Use a one-shot diff calculator for the final state advance of both Rust batch
import APIs. The previous persistent calculator forced general checkout mode
even for a causal linear history, causing large text batches to build and rewind
a richtext tracker. The existing DAG analysis can now select the existing linear
delta path; concurrency and shallow-history safety remain unchanged. There is no
size threshold, encoding change or CRDT algorithm change.

Preserve the transaction lock, one final state application, checkout notification,
pending reporting, decode-error behavior, and final-checkout rollback. Explicit
history checkout still uses its persistent calculator. Add deterministic mode
selection and contract regressions, and extend the native comparison example
with a large-text tail-replacement workload and sequential import as a third
strategy. Performance and allocation improvements are not claimed without
measurement. This is Rust-only and does not require an npm release entry.
