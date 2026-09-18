# Independent graph model

The oracle stores the raw test actions and **complete transitive ancestor sets**.
For each object and requested causal boundary it selects the maximal
Create/Delete/Restore events by pairwise happens-before comparisons; any maximal
Delete wins. Property actions never participate in that calculation. It does not
import production GraphOp, GraphState, lifecycle tags, diff calculators, or indexes.

`Action::Restore` means **ordinary explicit restore**, observing and withdrawing
all active deletes in its causal past. That behavior is equivalent to the
causal-maximal-event rule. UndoManager's selective recovery is different: it
targets only the local delete being undone, even when another peer's Delete has
already been observed. This oracle does not generate Undo/Redo or pretend that
selective undo is ordinary Restore. Independent remote deletion during selective
Undo is covered by the directed production contracts in `graph_native.rs`.

NodeId and EdgeId are distinct model types scoped to a graph. Their birth event
is mapped to the actual public API ID by the driver, never inferred from a name,
property value, or array position in a production query. Edges retain immutable
endpoints and an independent lifecycle. An alive edge is visible iff both
endpoints are alive. All adjacency, neighbor, and count expectations are derived
by scans of the normalized records.

Scalar properties use single-writer keys, including overwrite/removal. This avoids
duplicating the existing Map LWW implementation. Directed nested Text cases use
concurrent insertions at opposite ends of a shared nonempty string: the expected
text is independently `prefix + seed + suffix`. These cases check composition,
identity, and preservation while the graph object is deleted; they are not a
second general-purpose Text oracle.

## Driver contract

- Three replicas have distinct peer IDs. Every modeled action commits separately;
  its incremental update is retained as an independently deliverable packet.
- A receiver records delivered action IDs. Only the largest causally closed
  subset is evaluated. Missing dependencies stay pending, so an out-of-order
  delivery cannot silently teach the oracle facts taken from production state.
- Local mutations, partial synchronization, duplicates, reversed/shuffled packet
  groups, snapshots, checkout round trips, and historical forks all compare the
  public queries against the oracle. Forked editors receive fresh peer IDs.
- Capture an independent model version beside each real Frontiers boundary;
  checkout changes the expected boundary, not the oracle's raw history.
  Forward and reverse public `LoroDoc::diff` payloads are also reconstructed
  against the independently evaluated historical boundaries.
- Check exact typed ID sets, immutable endpoints, diagnostic alive/visible flags,
  properties, incoming/outgoing edge sets, deduplicated neighbors, and counts.
  Fresh snapshot imports exercise index reconstruction as well as incremental
  maintenance. Do not sort away duplicate query results before checking uniqueness.
- Event reconstruction uses only public GraphDiff payloads to update a separate
  visible graph and compares it with oracle/query state, especially for derived
  edge disappearance/reappearance after endpoint changes.

Use the already-resolved `rand 0.8.5` with `StdRng::seed_from_u64`; do not add a
property-test dependency. The default seeds are `1`, `7`, `42`,
`0xCA05_A1`, and `0xDE1E_7E`, with 120 randomized steps each. Start with a small
shared graph containing a diamond, a directed cycle, a self-loop, a parallel edge,
and a disconnected node. Generated scalar keys are per **peer ID**, including
fresh historical-fork peers, so a fork cannot turn them into concurrent LWW keys.

Select edits only from the oracle's local boundary, never from production queries.
Random traffic includes disconnected edits, one-way partial sends, shuffled
groups of 1–4 complete update packets, duplicate sends, and eventual full delivery.
These chunks are complete valid Loro update blobs, not arbitrary byte slices.
For each packet record the actual public VersionVector delta; compare DocState
version with the union of the expected applied deltas as an additional check that
the driver and the implementation agree about pending dependencies.

Replay controls are `LORO_GRAPH_SEED` (one decimal seed) and
`LORO_GRAPH_STEPS` (prefix length). On panic include seed, generated step index,
and the last 32 actions/deliveries with stable model IDs. A fixed seed and shorter
prefix reproduces the same generation decisions; always finish with delivery of
all generated packets and oracle comparisons. Shrink by finding the shortest
failing prefix, then promote the surviving schedule to a directed regression.
If an optional action-level reducer is added, delete the action **and its causal
dependents** together, preserve create/endpoint prerequisites, and remap later
selection indices; naive deletion of raw actions can make an invalid history.

Public API adapter needs: root graph access; typed create/delete/restore; full
record and visible node/edge iteration; per-record alive/visible/endpoints;
node/edge associated Map access; incoming/outgoing and neighbor queries; visible
counts; public GraphDiff fields. The adapter maps IDs and payloads only. It must
not read internal state/tags, derive oracle state from production snapshots, or
use production diff application to validate production events. Native CRUD,
binary format rejection fixtures, helper repair policies, and selective Undo
stay in the native suite; this suite supplies independent causal and composition
checks instead of duplicating those fixtures.

The adapter uses `loro/src/graph_api.rs`'s public `LoroGraph` API, and the driver
uses real `LoroDoc` imports/exports. The event mirror consumes `event::Diff::Graph`
records without applying production ops or recalculating endpoint visibility.
Graph event support is required; it must not be replaced with query-based mirror
refreshes to make the tests pass.

Files:

- `oracle.rs`: raw actions, transitive ancestors, historical normalization.
- `oracle_cases.rs`: model counterexamples, separate from production tests.
- `native.rs`: typed ID mapping, public query assertions, public event mirror.
- `driver.rs`: three real replicas, packet delivery, fork/checkout, snapshot merge
  and fresh-document reconstruction, bounded failure trace, shared
  `run_seed(seed, steps)` entry point.
- `regressions.rs`: the single saved seed-61516 pending/snapshot schedule, shared
  with the fuzz target (the directed schedule is not reproduced by a random seed
  alone).
- `differential.rs`: seeded randomized schedules and directed concurrent
  production/model tests (the partial-restore case runs
  separately for node and edge lifecycles).

## Running and replay

Run from the Loro repository root. The commands select the whole suite, only the
oracle cases, only the production differential cases, or one random seed:

```sh
CARGO_INCREMENTAL=0 cargo test -p loro --test graph_model -j 2
CARGO_INCREMENTAL=0 cargo test -p loro --test graph_model oracle_ -j 2
CARGO_INCREMENTAL=0 cargo test -p loro --test graph_model native_ -j 2
LORO_GRAPH_SEED=42 LORO_GRAPH_STEPS=120 CARGO_INCREMENTAL=0 cargo test -p loro --test graph_model native_random -j 2 -- --nocapture
```

Focused concurrent counterexamples live in `oracle_cases.rs`; the production
`graph_native.rs` suite owns ordinary CRUD/format fixtures and other native
contracts. The random schedules are bounded samples, not exhaustive state-space
exploration or a correctness proof. General Text edits, selective Undo/Redo,
malformed wire inputs, shallow history and helper repair policies are outside
this random model's coverage.

The existing fuzz crate's separate `graph` libFuzzer target includes this exact
oracle/driver source set. Its ten-byte seed/steps mapping, saved input corpus,
scope limits and short smoke commands are documented in
[`crates/fuzz/src/graph/README.md`](../../../fuzz/src/graph/README.md).
