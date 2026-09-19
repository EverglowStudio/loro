use wasm_bindgen::prelude::*;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(typescript_type = "'Graph'")]
    pub type JsGraphStr;
    #[wasm_bindgen(typescript_type = "GraphNodeId")]
    pub type JsGraphNodeId;
    #[wasm_bindgen(typescript_type = "GraphEdgeId")]
    pub type JsGraphEdgeId;
    #[wasm_bindgen(typescript_type = "GraphNodeId[]")]
    pub type JsGraphNodeIds;
    #[wasm_bindgen(typescript_type = "GraphEdgeId[]")]
    pub type JsGraphEdgeIds;
    #[wasm_bindgen(typescript_type = "GraphOrderTarget")]
    pub type JsGraphOrderTarget;
    #[wasm_bindgen(typescript_type = "GraphReorderOutcome")]
    pub type JsGraphReorderOutcome;
    #[wasm_bindgen(typescript_type = "OrderedGraphEdge[]")]
    pub type JsOrderedGraphEdges;
    #[wasm_bindgen(typescript_type = "OrderedGraphEdge | undefined")]
    pub type JsOrderedGraphEdgeOrUndefined;
    #[wasm_bindgen(typescript_type = "number")]
    pub type JsGraphOrderNumber;
    #[wasm_bindgen(typescript_type = "GraphNodeRecord | undefined")]
    pub type JsGraphNodeRecordOrUndefined;
    #[wasm_bindgen(typescript_type = "GraphEdgeRecord | undefined")]
    pub type JsGraphEdgeRecordOrUndefined;
    #[wasm_bindgen(typescript_type = "GraphNodeRecord[]")]
    pub type JsGraphNodeRecords;
    #[wasm_bindgen(typescript_type = "GraphEdgeRecord[]")]
    pub type JsGraphEdgeRecords;
    #[wasm_bindgen(typescript_type = "LoroGraph | undefined")]
    pub type JsGraphOrUndefined;
    #[wasm_bindgen(typescript_type = "GraphValue<ContainerID>")]
    pub type JsGraphShallowValue;
    #[wasm_bindgen(typescript_type = "GraphValue<Record<string, Value>>")]
    pub type JsGraphValue;
    #[wasm_bindgen(typescript_type = "GraphSnapshotEdge[]")]
    pub type JsGraphSnapshotEdges;
    #[wasm_bindgen(typescript_type = "GraphSnapshotValue")]
    pub type JsGraphSnapshotValue;
    #[wasm_bindgen(typescript_type = "GraphCycleReport")]
    pub type JsCycleReport;
    #[wasm_bindgen(typescript_type = "GraphRepairPolicy")]
    pub type JsGraphRepairPolicy;
    #[wasm_bindgen(typescript_type = "GraphRepairPlanValue")]
    pub type JsGraphRepairPlanValue;
}

#[wasm_bindgen(typescript_custom_section)]
const TYPES: &str = r#"
declare const graphNodeIdBrand: unique symbol;
declare const graphEdgeIdBrand: unique symbol;
/** Stable creation identity. The decimal peer component is never a JS number. */
export type GraphNodeId = `${number}@${PeerID}` & { readonly [graphNodeIdBrand]: true };
export type GraphEdgeId = `${number}@${PeerID}` & { readonly [graphEdgeIdBrand]: true };

/** Native canonical fractional-key hex, at most 4096 bytes and ending in 80.
 * Read-only diagnostic data; use ordering methods to allocate keys in Rust.
 */
export type GraphPosition = string;
/** Anchors select a gap at call time, not an enduring relationship to the anchor. */
export type GraphOrderTarget =
    | { type: "start" }
    | { type: "end" }
    | { type: "before"; edge: GraphEdgeId }
    | { type: "after"; edge: GraphEdgeId };
export type GraphReorderOutcome = {
    changed: boolean;
    /** Collision-opening writes to other edges; these compete under ordinary LWW. */
    auxiliaryUpdates: number;
};
export type OrderedGraphEdge = {
    edgeId: GraphEdgeId;
    target: GraphNodeId;
    position: GraphPosition;
};
/** Lossless operation identity and logical time; peer IDs stay inside the string. */
export type GraphOrderWriter = { id: JsonOpID; lamport: number };
export type GraphOrderValue = { position: GraphPosition; lastOrder: GraphOrderWriter };
export type GraphOrderDelta = {
    source: GraphNodeId;
    before: GraphOrderValue | null;
    after: GraphOrderValue | null;
};
export type GraphOrderErrorCode = "MissingNode" | "EdgeNotVisible" | "AnchorNotVisible" |
    "CrossSource" | "InvalidPosition" | "PositionTooLong" | "Engine" |
    "InvalidTarget" | "InvalidId" | "InvalidIndex" | "InvalidJitter";
/** Ordering failures are Error instances with a stable code field. */
export type GraphOrderError = Error & { code: GraphOrderErrorCode };

export type GraphNodeRecord = {
    id: GraphNodeId;
    alive: boolean;
    visible: boolean;
    /** Active deletion operation identities; restoring only clears observed tags. */
    deleteTags: JsonOpID[];
};
export type GraphEdgeRecord = {
    id: GraphEdgeId;
    source: GraphNodeId;
    target: GraphNodeId;
    position: GraphPosition;
    lastOrder: GraphOrderWriter;
    alive: boolean;
    /** False when deleted or when either endpoint is not alive. */
    visible: boolean;
    deleteTags: JsonOpID[];
};
/** Flat topology. Metadata can contain nested containers; graph edges remain IDs. */
export type GraphValue<M> = {
    nodes: { id: GraphNodeId; meta: M }[];
    edges: { id: GraphEdgeId; source: GraphNodeId; target: GraphNodeId; position: GraphPosition; meta: M }[];
};
export type GraphOp =
    | { type: "create_node"; id: GraphNodeId }
    | { type: "create_edge"; id: GraphEdgeId; source: GraphNodeId; target: GraphNodeId; position: GraphPosition }
    | { type: "set_edge_order"; id: GraphEdgeId; position: GraphPosition }
    | { type: "delete_node"; id: GraphNodeId }
    | { type: "delete_edge"; id: GraphEdgeId }
    | { type: "restore_node"; id: GraphNodeId; deletes: JsonOpID[] }
    | { type: "restore_edge"; id: GraphEdgeId; deletes: JsonOpID[] };
/** Graph operations in exportJsonUpdates/importJsonUpdates use the native enum shape. */
export type GraphJsonOp =
    | { create_node: { id: GraphNodeId } }
    | { create_edge: { id: GraphEdgeId; source: GraphNodeId; target: GraphNodeId; position: GraphPosition } }
    | { set_edge_order: { id: GraphEdgeId; position: GraphPosition } }
    | { delete_node: { id: GraphNodeId } }
    | { delete_edge: { id: GraphEdgeId } }
    | { restore_node: { id: GraphNodeId; deletes: JsonOpID[] } }
    | { restore_edge: { id: GraphEdgeId; deletes: JsonOpID[] } };
export type GraphDiff = {
    type: "graph";
    diff: {
        ops: { id: JsonOpID; lamport: number; op: GraphOp; forward: boolean }[];
        /** Null removes a creation record when checking out earlier history. */
        nodes: Record<GraphNodeId, GraphNodeRecord | null>;
        /** Includes derived visibility changes caused by endpoint delete/restore. */
        edges: Record<GraphEdgeId, GraphEdgeRecord | null>;
        /** Net register changes retain source and both writers for selective undo. */
        orders: Record<GraphEdgeId, GraphOrderDelta>;
    };
};

export type GraphSnapshotEdge = { id: GraphEdgeId; source: GraphNodeId; target: GraphNodeId };
export type GraphSnapshotValue = {
    graph: ContainerID;
    /** The captured DocState version, not the latest OpLog version. */
    version: OpId[];
    nodes: GraphNodeId[];
    edges: GraphSnapshotEdge[];
};
export type GraphScope = { type: "allVisible" } | { type: "selected"; edges: GraphEdgeId[] };
export type GraphCycleReport = {
    components: GraphNodeId[][];
    cyclicComponents: { nodes: GraphNodeId[]; witness: GraphEdgeId[] }[];
    selfLoops: GraphEdgeId[];
    scope: GraphScope;
    isAcyclic: boolean;
};
/** Deterministic SCC-local policy; does not promise a minimum feedback edge set. */
export type GraphRepairPolicy = "ascending-node-id-v1";
export type GraphRepairPlanValue = {
    graph: ContainerID;
    version: OpId[];
    policy: { id: "ascending-node-id"; version: 1 };
    scope: GraphScope;
    deletions: {
        edge: GraphEdgeId;
        reason: { type: "selfLoop" } | { type: "descendingEdgeInComponent"; component: number };
    }[];
};
export type GraphRepairErrorCode = "LoroError" | "UncommittedChanges" | "StalePlan" |
    "WrongGraph" | "DetachedContainer" | "InvalidSelection" | "InvalidPlan";
/** Helper failures are Error instances with a stable code field. */
export type GraphRepairError = Error & { code: GraphRepairErrorCode };

interface LoroGraph {
    subscribe(listener: Listener): Subscription;
}
"#;
