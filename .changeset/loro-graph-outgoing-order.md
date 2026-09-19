---
"loro-crdt": minor
---

Expose native LoroGraph outgoing-edge ordering through createEdgeAt, reorderEdge,
orderedOutEdges, outEdgeAt, indexOfOutEdge, and configureOrderJitter. The existing
createEdge method appends to the visible outgoing sequence. Ordering preserves
edge identity and metadata, supports deterministic collisions and native
selective undo, and reports structured errors for invalid targets and anchors.

Graph records and event/diff round trips retain canonical positions, Lamport
clocks, lossless full-width writer IDs, and net position changes. Collision-opening
auxiliary writes compete with concurrent moves under the same native LWW rule.
This updates the in-development Graph protocol; old Graph data is not migrated.
