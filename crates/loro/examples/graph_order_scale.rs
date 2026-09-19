//! Native outgoing-order measurements. Build once, then run each sample in a new process.
//! All reference sequences and diagnostics are outside the product-operation timers.
use loro::{
    ExportMode, Frontiers, GraphEdgeId, GraphNodeId, GraphOrderError, GraphOrderTarget as At,
    GraphReorderOutcome, LoroDoc, LoroGraph, VersionVector,
};
use serde_json::{json, Value};
use std::{collections::BTreeMap, hint::black_box, time::Instant};

const SCHEMA: &str = "lorograph-order-v1";
const HOT_GAP_INSERTS: usize = 256;
const NORMAL_MOVES: usize = 256;
const REPEATED_MOVES: usize = 4096;
const QUERY_LIMIT: usize = 4096;
const TARGET_POOL: usize = 32;
const HISTORY_ROUNDS: usize = 4;
const MAX_KEY_BYTES: usize = 4096;
const SHAPES: [&str; 6] = [
    "high-degree",
    "many-sources",
    "hot-gap",
    "repeated-moves",
    "deleted-history",
    "forced-collisions",
];
type Phases = BTreeMap<String, f64>;
type Stamp = (
    VersionVector,
    VersionVector,
    Frontiers,
    Frontiers,
    usize,
    usize,
);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Row {
    id: GraphEdgeId,
    source: GraphNodeId,
    target: GraphNodeId,
}

fn measure<T>(phases: &mut Phases, name: &str, f: impl FnOnce() -> T) -> T {
    let start = Instant::now();
    let result = f();
    *phases.entry(name.to_owned()).or_default() += start.elapsed().as_secs_f64() * 1000.0;
    result
}

fn stamp(doc: &LoroDoc) -> Stamp {
    (
        doc.oplog_vv(),
        doc.state_vv(),
        doc.oplog_frontiers(),
        doc.state_frontiers(),
        doc.len_ops(),
        doc.get_pending_txn_len(),
    )
}

fn document(peer: u64) -> LoroDoc {
    let doc = LoroDoc::new();
    doc.set_peer_id(peer).unwrap();
    doc
}

// Midpoint-first creation bounds initial key length. Slots describe caller intent;
// no product query, sort, or fractional-index implementation builds this oracle.
fn seed_domain(
    graph: &LoroGraph,
    source: GraphNodeId,
    targets: &[GraphNodeId],
    n: usize,
) -> Vec<Row> {
    fn fill(
        g: &LoroGraph,
        s: GraphNodeId,
        targets: &[GraphNodeId],
        slots: &mut [Option<Row>],
        offset: usize,
        upper: Option<GraphEdgeId>,
    ) {
        if slots.is_empty() {
            return;
        }
        let mid = slots.len() / 2;
        let target = targets[(offset + mid) % targets.len()];
        let id = g
            .create_edge_at(s, target, upper.map_or(At::End, At::Before))
            .unwrap();
        slots[mid] = Some(Row {
            id,
            source: s,
            target,
        });
        fill(g, s, targets, &mut slots[..mid], offset, Some(id));
        fill(
            g,
            s,
            targets,
            &mut slots[mid + 1..],
            offset + mid + 1,
            upper,
        );
    }
    let mut slots = vec![None; n];
    fill(graph, source, targets, &mut slots, 0, None);
    slots.into_iter().map(Option::unwrap).collect()
}

// Only key bytes are replaced in a genuine JSON operation history. IDs, counters,
// Lamports, endpoints, and dependencies remain intact. No document-per-edge clones.
fn rewrite_positions(
    doc: &LoroDoc,
    count: usize,
    key: impl Fn(usize) -> String,
) -> loro::JsonSchema {
    let mut value =
        serde_json::to_value(doc.export_json_updates_without_peer_compression(
            &VersionVector::default(),
            &doc.oplog_vv(),
        ))
        .unwrap();
    let mut replaced = 0;
    for change in value["changes"].as_array_mut().unwrap() {
        for op in change["ops"].as_array_mut().unwrap() {
            if let Some(body) = op["content"].get_mut("create_edge") {
                body["position"] = Value::String(key(replaced));
                replaced += 1;
            }
        }
    }
    assert_eq!(replaced, count);
    serde_json::from_value(value).unwrap()
}

fn check_order(graph: &LoroGraph, sources: &[GraphNodeId], expected: &[Vec<Row>]) {
    assert_eq!(sources.len(), expected.len());
    for (&source, rows) in sources.iter().zip(expected) {
        let actual = graph.ordered_out_edges(source).unwrap();
        assert_eq!(actual.len(), rows.len());
        for (entry, row) in actual.iter().zip(rows) {
            assert_eq!((entry.edge_id, entry.target), (row.id, row.target));
            assert_eq!(row.source, source);
            let key = entry.position.as_bytes();
            assert!(!key.is_empty() && key.len() <= MAX_KEY_BYTES && key.last() == Some(&128));
        }
        for pair in actual.windows(2) {
            assert!((&pair[0].position, pair[0].edge_id) < (&pair[1].position, pair[1].edge_id));
        }
        assert!(graph.out_edge_at(source, rows.len()).is_none());
        assert!(graph.out_edge_at(source, usize::MAX).is_none());
    }
    assert_eq!(
        graph.edge_count(),
        expected.iter().map(Vec::len).sum::<usize>()
    );
}

fn length_stats(mut lengths: Vec<usize>) -> Value {
    assert!(!lengths.is_empty());
    lengths.sort_unstable(); // Diagnostics only; never used to answer an order/rank/select query.
    let total: usize = lengths.iter().sum();
    json!({"count": lengths.len(), "total": total,
        "mean": total as f64 / lengths.len() as f64,
        "p95": lengths[(lengths.len() * 95).div_ceil(100) - 1],
        "max": lengths[lengths.len() - 1]})
}

fn key_stats(graph: &LoroGraph, rows: &[Row]) -> Value {
    let mut all = Vec::with_capacity(rows.len());
    let mut visible = Vec::new();
    for row in rows {
        let record = graph.edge_record(row.id).unwrap();
        assert_eq!((record.source, record.target), (row.source, row.target));
        let size = record.position.as_bytes().len();
        assert!((1..=MAX_KEY_BYTES).contains(&size));
        all.push(size);
        if record.visible {
            visible.push(size);
        }
    }
    json!({"all_records": length_stats(all), "visible": length_stats(visible)})
}

fn writes(calls: usize, changed: usize, auxiliary: &[usize]) -> Value {
    assert_eq!(calls, auxiliary.len());
    let mut histogram = BTreeMap::<usize, usize>::new();
    for count in auxiliary {
        *histogram.entry(*count).or_default() += 1;
    }
    json!({"calls": calls, "changed": changed, "primary_writes": changed,
        "auxiliary_total": auxiliary.iter().sum::<usize>(),
        "auxiliary_max": auxiliary.iter().max().copied().unwrap_or(0),
        "auxiliary_histogram": histogram})
}

fn read_queries(
    doc: &LoroDoc,
    graph: &LoroGraph,
    sources: &[GraphNodeId],
    expected: &[Vec<Row>],
    phases: &mut Phases,
) -> Value {
    let before = stamp(doc);
    let ordered = measure(phases, "ordered_query", || {
        sources
            .iter()
            .map(|s| black_box(graph.ordered_out_edges(*s).unwrap()))
            .collect::<Vec<_>>()
    });
    measure(phases, "behavior_validation", || {
        for (actual, rows) in ordered.iter().zip(expected) {
            assert_eq!(actual.len(), rows.len());
            for (entry, row) in actual.iter().zip(rows) {
                assert_eq!((entry.edge_id, entry.target), (row.id, row.target));
            }
        }
    });
    drop(ordered);
    let flat = expected
        .iter()
        .flat_map(|rows| rows.iter().enumerate().map(|(rank, row)| (rank, *row)))
        .collect::<Vec<_>>();
    let count = flat.len().min(QUERY_LIMIT);
    let queries: Vec<_> = (0..count)
        .map(|i| flat[i * (flat.len() - 1) / (count - 1)])
        .collect();
    let ranks = measure(phases, "rank_query", || {
        queries
            .iter()
            .map(|(_, row)| black_box(graph.index_of_out_edge(row.id)))
            .collect::<Vec<_>>()
    });
    let selected = measure(phases, "select_query", || {
        queries
            .iter()
            .map(|(rank, row)| black_box(graph.out_edge_at(row.source, *rank)))
            .collect::<Vec<_>>()
    });
    measure(phases, "behavior_validation", || {
        for (((rank, row), found), selected) in queries.iter().zip(ranks).zip(selected) {
            assert_eq!(found, Some(*rank));
            let selected = selected.unwrap();
            assert_eq!((selected.edge_id, selected.target), (row.id, row.target));
            assert_eq!(
                selected.position,
                graph.edge_record(row.id).unwrap().position
            );
        }
        check_order(graph, sources, expected);
        assert_eq!(
            stamp(doc),
            before,
            "read-only queries changed document history/transaction"
        );
    });
    json!({"ordered_calls": sources.len(), "ordered_edges": flat.len(),
        "rank_calls": count, "select_calls": count, "sample_selection": "evenly spaced, including both endpoints"})
}

fn materialize(doc: &LoroDoc, sources: &[GraphNodeId]) -> LoroGraph {
    let graph = doc.get_graph("graph");
    black_box(graph.edge_count());
    for source in sources {
        let _ = black_box(graph.out_edge_at(*source, 0));
    }
    graph
}

fn scale(n: usize, shape: &str) -> Value {
    let mut phases = Phases::new();
    let doc = document(41);
    let graph = doc.get_graph("graph");
    graph.configure_order_jitter(0);
    let seed_edges = n - match shape {
        "hot-gap" => HOT_GAP_INSERTS,
        "forced-collisions" => 1,
        _ => 0,
    };
    let source_count = if shape == "many-sources" {
        seed_edges.div_ceil(10)
    } else {
        1
    };
    let (sources, targets) = measure(&mut phases, "init_nodes", || {
        (
            (0..source_count)
                .map(|_| graph.create_node().unwrap())
                .collect::<Vec<_>>(),
            (0..TARGET_POOL)
                .map(|_| graph.create_node().unwrap())
                .collect::<Vec<_>>(),
        )
    });
    let mut expected = measure(&mut phases, "init_edges", || {
        sources
            .iter()
            .enumerate()
            .map(|(i, source)| {
                let degree = if source_count == 1 {
                    seed_edges
                } else {
                    (seed_edges - i * 10).min(10)
                };
                seed_domain(&graph, *source, &targets, degree)
            })
            .collect::<Vec<_>>()
    });
    measure(&mut phases, "init_commit", || doc.commit());
    assert_eq!(doc.len_ops(), source_count + TARGET_POOL + seed_edges);
    let mut collision_json_bytes = 0;
    let (doc, graph) = if shape == "forced-collisions" {
        let schema = measure(&mut phases, "collision_fixture_prepare", || {
            rewrite_positions(&doc, seed_edges, |_| "80".into())
        });
        collision_json_bytes = measure(&mut phases, "collision_fixture_json_encode", || {
            serde_json::to_vec(&schema).unwrap().len()
        });
        drop(graph);
        drop(doc);
        let doc = document(47);
        measure(&mut phases, "collision_fixture_import", || {
            doc.import_json_updates(schema).unwrap()
        });
        let graph = measure(&mut phases, "collision_fixture_materialize", || {
            materialize(&doc, &sources)
        });
        graph.configure_order_jitter(0);
        // The same-peer immutable ID tie-breaker is independently computed, outside query timing.
        expected[0].sort_unstable_by_key(|row| (row.id.peer, row.id.counter));
        assert_eq!(doc.len_ops(), source_count + TARGET_POOL + seed_edges);
        assert_eq!(
            doc.get_pending_txn_len(),
            0,
            "collision import performed repair writes"
        );
        for row in &expected[0] {
            assert_eq!(
                graph.edge_record(row.id).unwrap().position.as_bytes(),
                &[128]
            );
        }
        (doc, graph)
    } else {
        (doc, graph)
    };
    let initial_expected = expected.clone();
    let mut all_rows: Vec<_> = expected.iter().flatten().copied().collect();
    let initial_keys = measure(&mut phases, "key_diagnostics", || {
        key_stats(&graph, &all_rows)
    });
    measure(&mut phases, "behavior_validation", || {
        check_order(&graph, &sources, &expected)
    });
    let base_frontiers = doc.state_frontiers();
    let base_version = doc.oplog_vv();
    let base_ops = doc.len_ops();
    let base_snapshot = measure(&mut phases, "base_snapshot_encode", || {
        doc.export(ExportMode::snapshot()).unwrap()
    });
    let mut edits = BTreeMap::<&str, Value>::new();
    let mut special_assertions = BTreeMap::<&str, bool>::new();
    let mut lifecycle_ops = 0;
    let mut hidden_positions = Vec::new();

    if shape == "hot-gap" {
        let middle = expected[0].len() / 2;
        let right = expected[0][middle].id;
        let mut inserted = Vec::with_capacity(HOT_GAP_INSERTS);
        for i in 0..HOT_GAP_INSERTS {
            let pending = doc.get_pending_txn_len();
            let id = measure(&mut phases, "hot_gap_create", || {
                graph
                    .create_edge_at(sources[0], targets[i % TARGET_POOL], At::Before(right))
                    .unwrap()
            });
            assert_eq!(
                doc.get_pending_txn_len() - pending,
                1,
                "strict gap caused auxiliary writes"
            );
            inserted.push(Row {
                id,
                source: sources[0],
                target: targets[i % TARGET_POOL],
            });
        }
        all_rows.extend(inserted.iter().copied());
        expected[0].splice(middle..middle, inserted);
        measure(&mut phases, "hot_gap_commit", || doc.commit());
        edits.insert(
            "hot_gap_create",
            writes(HOT_GAP_INSERTS, HOT_GAP_INSERTS, &vec![0; HOT_GAP_INSERTS]),
        );
        check_order(&graph, &sources, &expected);
        special_assertions.insert("hot_gap_fixed_count_and_intent", true);
    }
    if shape == "deleted-history" {
        let hidden = n * 9 / 10;
        hidden_positions = all_rows[..hidden]
            .iter()
            .map(|r| graph.edge_record(r.id).unwrap().position)
            .collect();
        for _ in 0..HISTORY_ROUNDS {
            measure(&mut phases, "history_delete", || {
                for row in &all_rows[..hidden] {
                    graph.delete_edge(row.id).unwrap();
                }
            });
            measure(&mut phases, "history_commit", || doc.commit());
            assert_eq!(graph.edge_count(), n - hidden);
            measure(&mut phases, "history_restore", || {
                for row in &all_rows[..hidden] {
                    graph.restore_edge(row.id).unwrap();
                }
            });
            measure(&mut phases, "history_commit", || doc.commit());
            check_order(&graph, &sources, &expected);
        }
        measure(&mut phases, "history_final_delete", || {
            for row in &all_rows[..hidden] {
                graph.delete_edge(row.id).unwrap();
            }
        });
        measure(&mut phases, "history_commit", || doc.commit());
        lifecycle_ops = hidden * (2 * HISTORY_ROUNDS + 1);
        expected[0].drain(..hidden);
        for (row, position) in all_rows[..hidden].iter().zip(&hidden_positions) {
            let record = graph.edge_record(row.id).unwrap();
            assert!(!record.alive && !record.visible);
            assert_eq!(&record.position, position);
            assert!(graph.index_of_out_edge(row.id).is_none());
        }
        special_assertions.insert("deleted_history_retains_hidden_keys", true);
        special_assertions.insert("restore_preserves_order", true);
    }
    if shape == "forced-collisions" {
        let middle = seed_edges / 2;
        let moving = expected[0][seed_edges - 1];
        let result = measure(&mut phases, "collision_reorder", || {
            graph
                .reorder_edge(moving.id, At::Before(expected[0][middle].id))
                .unwrap()
        });
        assert!(result.changed);
        assert_eq!(result.auxiliary_updates, seed_edges - 1 - middle);
        expected[0].pop();
        expected[0].insert(middle, moving);
        measure(&mut phases, "collision_commit", || doc.commit());
        edits.insert(
            "collision_reorder",
            writes(1, 1, &[result.auxiliary_updates]),
        );
        check_order(&graph, &sources, &expected);
        let pending = doc.get_pending_txn_len();
        let id = measure(&mut phases, "collision_create", || {
            graph
                .create_edge_at(sources[0], targets[0], At::Before(expected[0][1].id))
                .unwrap()
        });
        let auxiliary = doc.get_pending_txn_len() - pending - 1;
        assert_eq!(auxiliary, middle - 1);
        let inserted = Row {
            id,
            source: sources[0],
            target: targets[0],
        };
        all_rows.push(inserted);
        expected[0].insert(1, inserted);
        measure(&mut phases, "collision_commit", || doc.commit());
        edits.insert("collision_create", writes(1, 1, &[auxiliary]));
        check_order(&graph, &sources, &expected);
        assert_eq!(
            graph
                .edge_record(expected[0][0].id)
                .unwrap()
                .position
                .as_bytes(),
            &[128]
        );
        special_assertions.insert("forced_equal_keys_and_id_tie_break", true);
        special_assertions.insert("collision_suffix_auxiliary_count", true);
        special_assertions.insert("collision_edit_intent", true);
        special_assertions.insert("collision_import_did_not_repair", true);
    }

    let move_count = if shape == "repeated-moves" {
        REPEATED_MOVES
    } else {
        NORMAL_MOVES
    };
    let plan = measure(&mut phases, "oracle_prepare", || {
        if shape == "repeated-moves" {
            let edge = expected[0].last().unwrap().id;
            // Even operation count returns the same edge to its initial final position.
            (0..move_count)
                .map(|i| (edge, if i % 2 == 0 { At::Start } else { At::End }))
                .collect::<Vec<_>>()
        } else {
            let plan = (0..move_count)
                .map(|i| {
                    let rows = &expected[i % source_count];
                    (rows[(i / source_count) % rows.len()].id, At::End)
                })
                .collect::<Vec<_>>();
            for (i, rows) in expected.iter_mut().enumerate() {
                let calls = move_count / source_count + usize::from(i < move_count % source_count);
                let offset = calls % rows.len();
                rows.rotate_left(offset);
            }
            plan
        }
    });
    let mut outcomes: Vec<GraphReorderOutcome> = Vec::with_capacity(move_count);
    for (i, (edge, at)) in plan.iter().enumerate() {
        outcomes.push(measure(&mut phases, "ordinary_reorder", || {
            graph.reorder_edge(*edge, *at).unwrap()
        }));
        if shape == "repeated-moves" {
            measure(&mut phases, "behavior_validation", || {
                let before = stamp(&doc);
                let index = if i % 2 == 0 { 0 } else { expected[0].len() - 1 };
                assert_eq!(graph.out_edge_at(sources[0], index).unwrap().edge_id, *edge);
                assert_eq!(graph.index_of_out_edge(*edge), Some(index));
                assert_eq!(stamp(&doc), before, "queries committed a pending reorder");
            });
        }
    }
    assert!(outcomes
        .iter()
        .all(|o| o.changed && o.auxiliary_updates == 0));
    if shape == "repeated-moves" {
        special_assertions.insert("repeated_move_each_destination", true);
    }
    edits.insert(
        "ordinary_reorder",
        writes(
            move_count,
            move_count,
            &outcomes
                .iter()
                .map(|o| o.auxiliary_updates)
                .collect::<Vec<_>>(),
        ),
    );
    measure(&mut phases, "ordinary_commit", || doc.commit());
    measure(&mut phases, "behavior_validation", || {
        check_order(&graph, &sources, &expected)
    });
    assert_eq!(all_rows.len(), n);
    for (row, position) in all_rows.iter().zip(&hidden_positions) {
        let record = graph.edge_record(row.id).unwrap();
        assert!(!record.alive && !record.visible);
        assert_eq!(
            &record.position, position,
            "ordinary reorder changed a hidden key"
        );
        assert!(graph.index_of_out_edge(row.id).is_none());
    }
    let before_noop = stamp(&doc);
    let last = expected[0].last().unwrap().id;
    let noop = measure(&mut phases, "noop_reorder", || {
        graph.reorder_edge(last, At::End).unwrap()
    });
    assert!(!noop.changed && noop.auxiliary_updates == 0);
    assert_eq!(stamp(&doc), before_noop);
    edits.insert("noop_reorder", writes(1, 0, &[0]));
    let edit_ops: usize = edits
        .values()
        .map(|v| {
            (v["primary_writes"].as_u64().unwrap() + v["auxiliary_total"].as_u64().unwrap())
                as usize
        })
        .sum();
    assert_eq!(doc.len_ops() - base_ops, edit_ops + lifecycle_ops);
    let query_counts = read_queries(&doc, &graph, &sources, &expected, &mut phases);
    let final_keys = measure(&mut phases, "key_diagnostics", || {
        key_stats(&graph, &all_rows)
    });
    let final_records = measure(&mut phases, "behavior_validation", || graph.edge_records());
    assert_eq!(final_records.len(), n);
    let final_nodes = graph.node_records();
    let before_exports = stamp(&doc);
    let updates = measure(&mut phases, "updates_encode", || {
        doc.export(ExportMode::all_updates()).unwrap()
    });
    let delta = measure(&mut phases, "edit_delta_encode", || {
        doc.export(ExportMode::updates(&base_version)).unwrap()
    });
    let snapshot = measure(&mut phases, "snapshot_encode", || {
        doc.export(ExportMode::snapshot()).unwrap()
    });
    assert_eq!(
        stamp(&doc),
        before_exports,
        "export changed document history"
    );

    // Receivers are scoped sequentially. At most two documents coexist, independent of E.
    for mode in ["updates", "snapshot", "delta"] {
        let receiver = document(53);
        if mode == "delta" {
            measure(&mut phases, "delta_base_import", || {
                receiver.import(&base_snapshot).unwrap()
            });
            measure(&mut phases, "delta_base_materialize", || {
                materialize(&receiver, &sources)
            });
        }
        let bytes = match mode {
            "updates" => &updates,
            "snapshot" => &snapshot,
            _ => &delta,
        };
        measure(&mut phases, &format!("{mode}_import"), || {
            receiver.import(bytes).unwrap()
        });
        let loaded = measure(
            &mut phases,
            &format!("{mode}_materialize_first_select"),
            || materialize(&receiver, &sources),
        );
        measure(&mut phases, "behavior_validation", || {
            assert_eq!(receiver.oplog_vv(), doc.oplog_vv());
            assert_eq!(receiver.get_pending_txn_len(), 0);
            assert_eq!(loaded.edge_records(), final_records);
            assert_eq!(loaded.node_records(), final_nodes);
            check_order(&loaded, &sources, &expected);
        });
        if mode == "snapshot" {
            let before = receiver.len_ops();
            let result = measure(&mut phases, "snapshot_continue_edit", || {
                loaded.reorder_edge(last, At::Start).unwrap()
            });
            assert!(result.changed);
            receiver.commit();
            let mut touched = expected[0].clone();
            let row = touched.pop().unwrap();
            touched.insert(0, row);
            check_order_domain(&loaded, sources[0], &touched);
            assert_eq!(receiver.len_ops() - before, 1 + result.auxiliary_updates);
        }
    }
    measure(&mut phases, "checkout_base_rebuild", || {
        doc.checkout(&base_frontiers).unwrap()
    });
    measure(&mut phases, "checkout_base_materialize", || {
        materialize(&doc, &sources)
    });
    measure(&mut phases, "behavior_validation", || {
        check_order(&graph, &sources, &initial_expected)
    });
    measure(&mut phases, "checkout_latest_rebuild", || doc.attach());
    measure(&mut phases, "checkout_latest_materialize", || {
        materialize(&doc, &sources)
    });
    measure(&mut phases, "behavior_validation", || {
        check_order(&graph, &sources, &expected);
        assert_eq!(graph.edge_records(), final_records);
        assert_eq!(stamp(&doc), before_exports);
    });
    let mut assertions = BTreeMap::from([
        ("input_edge_count", true),
        ("native_order_matches_intent", true),
        ("stable_identity_endpoints", true),
        ("canonical_bounded_keys", true),
        ("ordinary_reorder_no_auxiliary", true),
        ("noop_no_operations", true),
        ("operation_accounting", true),
        ("rank_select_match_intent", true),
        ("queries_read_only", true),
        ("exports_read_only", true),
        ("updates_roundtrip", true),
        ("snapshot_roundtrip", true),
        ("delta_roundtrip", true),
        ("snapshot_continue_edit", true),
        ("checkout_rebuild_matches_intent", true),
    ]);
    assertions.extend(special_assertions);
    json!({
        "schema": SCHEMA, "kind": "scale", "release": !cfg!(debug_assertions),
        "shape": shape, "edges": n, "seed_edges": seed_edges, "edge_records": all_rows.len(),
        "visible_edges": graph.edge_count(), "nodes": source_count + TARGET_POOL,
        "sources": source_count, "target_pool": TARGET_POOL, "jitter": 0,
        "init_strategy": "midpoint-first public create_edge_at",
        "hot_gap_operations": if shape == "hot-gap" { HOT_GAP_INSERTS } else { 0 },
        "history_rounds": if shape == "deleted-history" { HISTORY_ROUNDS } else { 0 },
        "operations": {"initial": base_ops, "final": doc.len_ops(), "lifecycle": lifecycle_ops, "edits": edit_ops},
        "queries": query_counts, "writes": edits,
        "key_bytes": {"initial": initial_keys, "final": final_keys},
        "encoded_bytes": {"base_snapshot": base_snapshot.len(), "updates": updates.len(),
            "edit_delta": delta.len(), "snapshot": snapshot.len(), "collision_json_fixture": collision_json_bytes},
        "phases_ms": phases, "assertions": assertions,
        "max_live_documents": 2,
        "notes": "native API timings exclude reference planning/validation and explicit commits; whole-process CPU/RSS includes fixture, oracle, diagnostics, serialization and sequential receivers; no latency p95 claim"
    })
}

fn check_order_domain(graph: &LoroGraph, source: GraphNodeId, rows: &[Row]) {
    let actual = graph.ordered_out_edges(source).unwrap();
    assert_eq!(actual.len(), rows.len());
    for (entry, row) in actual.iter().zip(rows) {
        assert_eq!((entry.edge_id, entry.target), (row.id, row.target));
    }
}

// A directed resource-boundary test, deliberately excluded from the scale matrix.
// Seed a valid near-limit common prefix, then keep inserting into one shrinking gap.
fn boundary() -> Value {
    let mut phases = Phases::new();
    let seed = document(41);
    let graph = seed.get_graph("graph");
    let source = graph.create_node().unwrap();
    let left = graph.create_edge_at(source, source, At::End).unwrap();
    let right = graph.create_edge_at(source, source, At::End).unwrap();
    seed.commit();
    let prefix = "80".repeat(MAX_KEY_BYTES - 3);
    let schema = rewrite_positions(&seed, 2, |i| {
        format!("{prefix}{}", if i == 0 { "80" } else { "8180" })
    });
    drop(graph);
    drop(seed);
    let doc = document(47);
    doc.import_json_updates(schema).unwrap();
    let graph = doc.get_graph("graph");
    graph.configure_order_jitter(0);
    let mut rows = vec![Row {
        id: left,
        source,
        target: source,
    }];
    let mut max_bytes = 0;
    let mut failed = false;
    for _ in 0..4096 {
        let before = stamp(&doc);
        let count = graph.edge_count();
        match measure(&mut phases, "boundary_insert_attempts", || {
            graph.create_edge_at(source, source, At::Before(right))
        }) {
            Ok(id) => {
                let size = graph.edge_record(id).unwrap().position.as_bytes().len();
                assert!(size <= MAX_KEY_BYTES);
                max_bytes = max_bytes.max(size);
                rows.push(Row {
                    id,
                    source,
                    target: source,
                });
                doc.commit();
            }
            Err(GraphOrderError::PositionTooLong) => {
                assert_eq!(stamp(&doc), before);
                assert_eq!(graph.edge_count(), count);
                failed = true;
                break;
            }
            Err(other) => panic!("unexpected boundary failure: {other}"),
        }
    }
    assert!(
        failed,
        "did not reach the bounded key error within the directed test limit"
    );
    assert_eq!(max_bytes, MAX_KEY_BYTES);
    let successful = rows.len() - 1;
    let last_id = rows.last().unwrap().id;
    assert!(successful > 0);
    rows.push(Row {
        id: right,
        source,
        target: source,
    });
    check_order(&graph, &[source], &[rows.clone()]);
    let next = graph.create_edge_at(source, source, At::End).unwrap();
    assert_eq!(next.peer, last_id.peer);
    assert_eq!(
        next.counter,
        last_id.counter + 1,
        "failed insertion consumed an operation identity"
    );
    rows.push(Row {
        id: next,
        source,
        target: source,
    });
    doc.commit();
    check_order(&graph, &[source], &[rows.clone()]);
    json!({"schema": SCHEMA, "kind": "boundary", "release": !cfg!(debug_assertions),
        "common_prefix_bytes": MAX_KEY_BYTES - 3, "successful_insertions": successful,
        "attempts": successful + 1, "max_key_bytes": max_bytes, "edges": rows.len(),
        "phases_ms": phases,
        "assertions": {"reached_4096_bytes": true, "position_too_long_error": true,
            "failure_no_partial_writes": true, "failure_no_id_consumption": true,
            "accepted_order_matches_intent": true, "subsequent_edit_succeeds": true},
        "notes": "directed near-limit valid JSON fixture; not a natural-growth or scale-performance sample"})
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args == ["--boundary-limit"] {
        println!("{}", boundary());
    } else if args.len() == 2 {
        let n: usize = args[0].parse()?;
        if ![1000, 10000, 100000].contains(&n) || !SHAPES.contains(&args[1].as_str()) {
            return Err("expected edges=1000|10000|100000 and a supported shape".into());
        }
        println!("{}", scale(n, &args[1]));
    } else {
        return Err("usage: graph_order_scale EDGES SHAPE | --boundary-limit".into());
    }
    Ok(())
}
