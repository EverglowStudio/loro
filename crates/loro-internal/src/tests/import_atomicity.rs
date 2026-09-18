use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use crate::{
    cursor::PosType,
    encoding::json_schema::json::{JsonOpContent, JsonSchema, ListOp},
    loro::ExportMode,
    state::fail_next_import_state_apply_for_test,
    version::{Frontiers, VersionVector},
    LoroDoc, LoroError, TreeParentId,
};

fn pending_len(doc: &LoroDoc) -> usize {
    doc.oplog().lock().pending_changes_len()
}

fn corrupt_snapshot_state_bytes(snapshot: &mut [u8]) {
    let body_start = 22;
    let oplog_len =
        u32::from_le_bytes(snapshot[body_start..body_start + 4].try_into().unwrap()) as usize;
    let state_len_pos = body_start + 4 + oplog_len;
    let state_len = u32::from_le_bytes(
        snapshot[state_len_pos..state_len_pos + 4]
            .try_into()
            .unwrap(),
    ) as usize;
    assert!(state_len > 0);
    let state_start = state_len_pos + 4;
    snapshot[state_start] ^= 0xff;

    refresh_snapshot_checksum(snapshot);
}

#[derive(Clone, Copy, Debug)]
enum SnapshotKvSection {
    OpLog,
    State,
}

fn corrupt_snapshot_sstable_block(snapshot: &mut [u8], section: SnapshotKvSection) {
    let body_start = 22;
    let oplog_len =
        u32::from_le_bytes(snapshot[body_start..body_start + 4].try_into().unwrap()) as usize;
    let oplog_start = body_start + 4;
    let state_len_pos = oplog_start + oplog_len;
    let state_len = u32::from_le_bytes(
        snapshot[state_len_pos..state_len_pos + 4]
            .try_into()
            .unwrap(),
    ) as usize;
    let state_start = state_len_pos + 4;
    let (section_start, section_len) = match section {
        SnapshotKvSection::OpLog => (oplog_start, oplog_len),
        SnapshotKvSection::State => (state_start, state_len),
    };

    // SSTable layout starts with four magic bytes and one schema byte. Corrupt the first block
    // payload while leaving its embedded checksum stale, then refresh only the outer envelope.
    assert!(section_len > 9);
    snapshot[section_start + 5] ^= 0xff;
    refresh_snapshot_checksum(snapshot);
}

fn refresh_snapshot_checksum(snapshot: &mut [u8]) {
    let checksum = xxhash_rust::xxh32::xxh32(&snapshot[20..], u32::from_le_bytes(*b"LORO"));
    snapshot[16..20].copy_from_slice(&checksum.to_le_bytes());
}

fn make_json_list_update_with_four_ops(peer: u64) -> (LoroDoc, JsonSchema) {
    let doc = LoroDoc::new();
    doc.set_peer_id(peer).unwrap();
    let map = doc.get_map("map");
    let list = doc.get_list("list");
    let text = doc.get_text("text");

    let mut txn = doc.txn().unwrap();
    map.insert_with_txn(&mut txn, "prefix", "map-value".into())
        .unwrap();
    list.insert_with_txn(&mut txn, 0, "seed".into()).unwrap();
    text.insert_with_txn(&mut txn, 0, "text-value", PosType::Unicode)
        .unwrap();
    list.insert_with_txn(&mut txn, 1, "tail".into()).unwrap();
    txn.commit().unwrap();

    let json = doc.export_json_updates(&Default::default(), &doc.oplog_vv(), false);
    assert_eq!(json.changes.len(), 1);
    assert_eq!(json.changes[0].ops.len(), 4);
    (doc, json)
}

fn move_last_list_insert_far_out_of_bounds(json: &mut JsonSchema) {
    let last_change = json.changes.last_mut().unwrap();
    let last_op = last_change.ops.last_mut().unwrap();
    match &mut last_op.content {
        JsonOpContent::List(ListOp::Insert { pos, .. }) => {
            *pos = 1_000;
        }
        other => panic!("expected list insert op, got {other:?}"),
    }
}

fn make_multi_peer_frontier_doc() -> LoroDoc {
    let base = LoroDoc::new_auto_commit();
    base.set_peer_id(1).unwrap();
    base.get_map("map").insert("base", 0).unwrap();
    base.get_text("text").insert_unicode(0, "base").unwrap();
    let tree = base.get_tree("tree");
    let root = tree.create(TreeParentId::Root).unwrap();
    tree.get_meta(root).unwrap().insert("base", 0).unwrap();

    let base_updates = base.export(ExportMode::all_updates()).unwrap();

    let peer2 = base.fork();
    peer2.set_peer_id(2).unwrap();
    peer2.get_map("map").insert("p2", 2).unwrap();
    peer2.get_text("text").insert_unicode(0, "p2").unwrap();
    peer2.commit_then_renew();
    let peer2_updates = peer2.export(ExportMode::updates(&base.oplog_vv())).unwrap();

    let peer3 = base.fork();
    peer3.set_peer_id(3).unwrap();
    peer3.get_map("map").insert("p3", 3).unwrap();
    let peer3_tree = peer3.get_tree("tree");
    let node = peer3_tree.create(TreeParentId::Root).unwrap();
    peer3_tree.get_meta(node).unwrap().insert("p3", 3).unwrap();
    peer3.commit_then_renew();
    let peer3_updates = peer3.export(ExportMode::updates(&base.oplog_vv())).unwrap();

    let target = LoroDoc::new();
    target.import(&base_updates).unwrap();
    target.import(&peer2_updates).unwrap();
    target.import(&peer3_updates).unwrap();
    target
}

fn assert_doc_unchanged(
    doc: &LoroDoc,
    vv: &VersionVector,
    frontiers: &Frontiers,
    state: &crate::LoroValue,
) {
    assert_eq!(&doc.oplog_vv(), vv);
    assert_eq!(&doc.oplog_frontiers(), frontiers);
    assert_eq!(&doc.get_deep_value(), state);
}

#[test]
fn failed_dependency_import_rolls_back_single_pending_change() {
    let src = LoroDoc::new_auto_commit();
    src.set_peer_id(1).unwrap();
    let map = src.get_map("map");
    map.insert("seed", "base").unwrap();
    let update_base = src
        .export(ExportMode::updates(&VersionVector::default()))
        .unwrap();
    let version_base = src.oplog_vv();

    map.insert("later", "value").unwrap();
    let update_later = src.export(ExportMode::updates(&version_base)).unwrap();

    let dst = LoroDoc::new();
    dst.import(&update_later).unwrap();
    assert_eq!(pending_len(&dst), 1);
    let vv_before_import = dst.oplog_vv();
    let frontiers_before_import = dst.oplog_frontiers();
    let state_before_import = dst.get_deep_value();

    fail_next_import_state_apply_for_test();
    let err = dst.import(&update_base).unwrap_err();
    assert!(
        err.to_string().contains("state apply failpoint"),
        "unexpected error: {err:?}"
    );
    assert_eq!(pending_len(&dst), 1);
    assert_eq!(dst.oplog_vv(), vv_before_import);
    assert_eq!(dst.oplog_frontiers(), frontiers_before_import);
    assert_eq!(dst.get_deep_value(), state_before_import);

    dst.import(&update_base).unwrap();
    assert_eq!(pending_len(&dst), 0);
    assert_eq!(dst.oplog_vv(), src.oplog_vv());
    assert_eq!(dst.oplog_frontiers(), src.oplog_frontiers());
    assert_eq!(dst.get_deep_value(), src.get_deep_value());
}

#[test]
fn failed_dependency_import_rolls_back_multiple_pending_changes() {
    let base = LoroDoc::new_auto_commit();
    base.set_peer_id(1).unwrap();
    let map = base.get_map("map");
    map.insert("seed", "base").unwrap();
    let update_base = base
        .export(ExportMode::updates(&VersionVector::default()))
        .unwrap();
    let version_base = base.oplog_vv();

    let peer2 = LoroDoc::new_auto_commit();
    peer2.set_peer_id(2).unwrap();
    peer2.import(&update_base).unwrap();
    peer2.get_map("map").insert("p2", "B").unwrap();
    let update_peer2 = peer2.export(ExportMode::updates(&version_base)).unwrap();

    let peer3 = LoroDoc::new_auto_commit();
    peer3.set_peer_id(3).unwrap();
    peer3.import(&update_base).unwrap();
    peer3.get_map("map").insert("p3", "C").unwrap();
    let update_peer3 = peer3.export(ExportMode::updates(&version_base)).unwrap();

    let expected = LoroDoc::new();
    expected.import(&update_base).unwrap();
    expected.import(&update_peer2).unwrap();
    expected.import(&update_peer3).unwrap();

    let dst = LoroDoc::new();
    dst.import(&update_peer2).unwrap();
    dst.import(&update_peer3).unwrap();
    assert_eq!(pending_len(&dst), 2);

    let vv_before_import = dst.oplog_vv();
    let frontiers_before_import: Frontiers = dst.oplog_frontiers();
    let state_before_import = dst.get_deep_value();

    fail_next_import_state_apply_for_test();
    let err = dst.import(&update_base).unwrap_err();
    assert!(
        err.to_string().contains("state apply failpoint"),
        "unexpected error: {err:?}"
    );
    assert_eq!(pending_len(&dst), 2);
    assert_eq!(dst.oplog_vv(), vv_before_import);
    assert_eq!(dst.oplog_frontiers(), frontiers_before_import);
    assert_eq!(dst.get_deep_value(), state_before_import);

    dst.import(&update_base).unwrap();
    assert_eq!(pending_len(&dst), 0);
    assert_eq!(dst.oplog_vv(), expected.oplog_vv());
    assert_eq!(dst.oplog_frontiers(), expected.oplog_frontiers());
    assert_eq!(dst.get_deep_value(), expected.get_deep_value());
}

#[test]
fn failed_import_keeps_multi_peer_frontiers_intact() {
    let target = make_multi_peer_frontier_doc();
    let vv_before_import = target.oplog_vv();
    assert!(vv_before_import.iter().count() >= 3);
    let frontiers_before_import = target.oplog_frontiers();
    let state_before_import = target.get_deep_value();

    let (_, mut bad_json) = make_json_list_update_with_four_ops(4);
    move_last_list_insert_far_out_of_bounds(&mut bad_json);
    let bad_json = serde_json::to_string(&bad_json).unwrap();

    let err = target.import_json_updates(&bad_json).unwrap_err();
    assert!(
        err.to_string().contains("list diff"),
        "expected state list bounds validation, got {err:?}"
    );
    assert_eq!(target.oplog_vv(), vv_before_import);
    assert_eq!(target.oplog_frontiers(), frontiers_before_import);
    assert_eq!(target.get_deep_value(), state_before_import);
}

#[test]
fn malformed_json_import_returns_error_without_mutating_doc() {
    let doc = make_multi_peer_frontier_doc();
    let vv_before_import = doc.oplog_vv();
    let frontiers_before_import = doc.oplog_frontiers();
    let state_before_import = doc.get_deep_value();

    let err = doc
        .import_json_updates("[3,{ \"('  k\" :\n\n42222 }]")
        .unwrap_err();
    assert_eq!(err, LoroError::InvalidJsonSchema);
    assert_doc_unchanged(
        &doc,
        &vv_before_import,
        &frontiers_before_import,
        &state_before_import,
    );
}

#[test]
fn failed_import_does_not_emit_events() {
    let doc = LoroDoc::new();
    let hit = Arc::new(AtomicUsize::new(0));
    let hit_cloned = hit.clone();
    let _sub = doc.subscribe_root(Arc::new(move |_event| {
        hit_cloned.fetch_add(1, Ordering::SeqCst);
    }));

    let (_, mut bad_json) = make_json_list_update_with_four_ops(7);
    move_last_list_insert_far_out_of_bounds(&mut bad_json);
    let bad_json = serde_json::to_string(&bad_json).unwrap();
    let err = doc.import_json_updates(&bad_json).unwrap_err();
    assert!(
        err.to_string().contains("list diff"),
        "expected state list bounds validation, got {err:?}"
    );
    assert_eq!(hit.load(Ordering::SeqCst), 0);
    assert!(doc.drop_pending_events().is_empty());

    let (_, good_json) = make_json_list_update_with_four_ops(8);
    let good_json = serde_json::to_string(&good_json).unwrap();
    doc.import_json_updates(&good_json).unwrap();
    assert!(hit.load(Ordering::SeqCst) > 0);
}

/// Build a binary update blob that decodes into the `OpLog` fine but whose ops are
/// rejected by `DocState` validation (list insert far out of bounds).
fn malformed_binary_update(peer: u64) -> Vec<u8> {
    let (_, mut bad_json) = make_json_list_update_with_four_ops(peer);
    move_last_list_insert_far_out_of_bounds(&mut bad_json);

    // A detached doc records changes into the OpLog without applying them to state,
    // so the malformed op survives into the exported binary updates.
    let carrier = LoroDoc::new();
    carrier.detach();
    carrier
        .import_json_updates(serde_json::to_string(&bad_json).unwrap())
        .unwrap();
    carrier.export(ExportMode::all_updates()).unwrap()
}

/// The endpoint exists at the receiver, but the writer did not observe its creation.
/// State membership alone accepts this edge; the operation DAG must reject it.
fn causally_invalid_graph_update(peer: u64, unobserved: loro_common::GraphNodeId) -> Vec<u8> {
    use crate::container::graph::GraphOp;

    let source = LoroDoc::new_auto_commit();
    source.set_peer_id(peer).unwrap();
    let graph = source.get_graph("graph");
    let node = graph.create_node().unwrap();
    graph.create_edge(node, node).unwrap();
    source.commit_then_renew();
    let mut schema = source.export_json_updates(&Default::default(), &source.oplog_vv(), false);
    let mut changed = false;
    for change in &mut schema.changes {
        for op in &mut change.ops {
            if let JsonOpContent::Graph(GraphOp::CreateEdge { source, .. }) = &mut op.content {
                *source = unobserved;
                changed = true;
            }
        }
    }
    assert!(changed);

    // Deliberately bypass import validation only while constructing the malformed
    // fixture. The test imports ordinary, checksummed binary updates through the API.
    let carrier = LoroDoc::new();
    carrier.detach();
    {
        let mut oplog = carrier.oplog().lock();
        let changes =
            crate::encoding::json_schema::decode_json_changes(schema, &oplog.arena).unwrap();
        // Load committed remote history; import_local_change requires an active
        // transaction's pending DAG node and cannot construct this fixture.
        let imported = crate::encoding::import_changes_unchecked_for_test(changes, &mut oplog);
        assert!(imported.pending_changes.is_empty());
        assert!(imported
            .changes_that_have_deps_before_shallow_root
            .is_empty());
        assert_eq!(oplog.vv(), &source.oplog_vv());
    }
    carrier.export(ExportMode::all_updates()).unwrap()
}

#[test]
fn batch_graph_validation_failure_preserves_outer_rollback_and_pending() {
    for updates_only in [false, true] {
        for trailing_state_error in [false, true] {
            let (dst, chain, expected) = doc_with_snapshot_and_pending_updates();
            let node = dst.get_graph("graph").create_node().unwrap();
            dst.commit_then_renew();
            dst.import(&chain[0]).unwrap();
            assert_eq!(pending_len(&dst), 1);
            let pending_before = dst.oplog().lock().pending_changes.version_range();
            let vv_before = dst.oplog_vv();
            let frontiers_before = dst.oplog_frontiers();
            let state_frontiers_before = dst.state_frontiers();
            let value_before = dst.get_deep_value();

            let mut blobs = vec![causally_invalid_graph_update(41, node)];
            // These updates park and unlock batch-local entries, and unlock the
            // pre-batch entry. All of that must remain in the outer journal.
            blobs.extend(chain.iter().skip(1).cloned());
            if trailing_state_error {
                blobs.push(malformed_binary_update(42));
            }
            let error = import_test_batch(&dst, &blobs, updates_only).unwrap_err();
            assert!(error.to_string().contains("Graph"), "{error:?}");
            assert!(!dst.is_detached());
            assert_doc_unchanged(&dst, &vv_before, &frontiers_before, &value_before);
            assert_eq!(dst.state_frontiers(), state_frontiers_before);
            assert_eq!(pending_len(&dst), 1);
            {
                let oplog = dst.oplog().lock();
                assert_eq!(oplog.pending_changes.version_range(), pending_before);
                assert!(!oplog.batch_importing);
                assert!(!oplog.has_import_rollback());
            }

            // The preserved pending entry must still unlock on a valid retry.
            import_test_batch(&dst, &chain, updates_only).unwrap();
            assert_eq!(pending_len(&dst), 0);
            assert_eq!(dst.oplog_vv().get(&2), expected.oplog_vv().get(&2));
            assert_eq!(dst.get_graph("graph").nodes(), vec![node]);
            assert_eq!(dst.get_graph("graph").edge_count(), 0);
            dst.get_map("map")
                .insert("after_graph_rejection", true)
                .unwrap();
            dst.commit_then_renew();
            assert_eq!(dst.state_frontiers(), dst.oplog_frontiers());
            let copy = LoroDoc::new();
            copy.import(&dst.export(ExportMode::Snapshot).unwrap())
                .unwrap();
            assert_eq!(copy.get_deep_value(), dst.get_deep_value());
        }
    }
}

#[test]
fn detached_graph_batch_rejection_rolls_back_its_own_scope() {
    let dst = LoroDoc::new_auto_commit();
    dst.set_peer_id(3).unwrap();
    let node = dst.get_graph("graph").create_node().unwrap();
    dst.commit_then_renew();
    dst.detach();
    let vv_before = dst.oplog_vv();
    let frontiers_before = dst.oplog_frontiers();
    let value_before = dst.get_deep_value();
    let bad = causally_invalid_graph_update(41, node);
    assert!(dst.import_updates_batch(&[&bad]).is_err());
    assert!(dst.is_detached());
    assert_doc_unchanged(&dst, &vv_before, &frontiers_before, &value_before);
    assert_eq!(dst.state_frontiers(), frontiers_before);
    assert_eq!(pending_len(&dst), 0);
    assert!(!dst.oplog().lock().has_import_rollback());
    assert!(!dst.oplog().lock().batch_importing);
    dst.checkout_to_latest();
    dst.get_graph("graph").create_node().unwrap();
    dst.commit_then_renew();
    assert_eq!(dst.state_frontiers(), dst.oplog_frontiers());
}

fn doc_with_snapshot_and_pending_updates() -> (LoroDoc, Vec<Vec<u8>>, LoroDoc) {
    let base = LoroDoc::new_auto_commit();
    base.set_peer_id(1).unwrap();
    base.get_text("text").insert_unicode(0, "base").unwrap();
    base.get_list("list").push("base").unwrap();
    base.commit_then_renew();
    let snapshot = base.export(ExportMode::Snapshot).unwrap();
    let base_vv = base.oplog_vv();

    // A chain of updates from a second peer. Imported out of order, all but the
    // first land in `pending_changes` until their deps arrive.
    let peer2 = base.fork();
    peer2.set_peer_id(2).unwrap();
    let mut chain = Vec::new();
    let mut from = base_vv.clone();
    for i in 0..5 {
        peer2.get_map("map").insert(&format!("k{i}"), i).unwrap();
        peer2.commit_then_renew();
        chain.push(peer2.export(ExportMode::updates(&from)).unwrap());
        from = peer2.oplog_vv();
    }
    // Latest first: everything but the last blob stays pending while the batch runs.
    chain.reverse();

    let dst = LoroDoc::new_auto_commit();
    dst.set_peer_id(3).unwrap();
    dst.import(&snapshot).unwrap();
    (dst, chain, peer2)
}

/// A blob whose ops only fail when they reach `DocState` used to panic inside the
/// batch reattach (`checkout to oplog frontiers should succeed`), unwinding past the
/// reattach and leaving the document detached — and every later import/export broken.
#[test]
fn import_batch_with_unappliable_update_stays_attached_and_rolls_back() {
    for updates_only in [false, true] {
        let (dst, mut blobs, _) = doc_with_snapshot_and_pending_updates();
        blobs.insert(0, malformed_binary_update(21));

        let vv_before = dst.oplog_vv();
        let frontiers_before = dst.oplog_frontiers();
        let state_before = dst.get_deep_value();

        let err = import_test_batch(&dst, &blobs, updates_only)
            .expect_err("a state-rejected op must fail the batch");
        assert!(
            err.to_string().contains("list diff"),
            "expected state list bounds validation, got {err:?}"
        );

        assert!(
            !dst.is_detached(),
            "import_batch must leave the doc attached"
        );
        assert!(!dst.oplog().lock().batch_importing);
        assert_doc_unchanged(&dst, &vv_before, &frontiers_before, &state_before);
        // The chain blobs were parked and then unlocked *within* the batch; its rollback
        // must not resurrect them. A resurrected entry would carry `ContainerIdx`
        // registrations the arena rollback just truncated.
        assert_eq!(
            pending_len(&dst),
            0,
            "changes the batch itself parked must not survive its rollback"
        );

        // Still fully usable afterwards.
        dst.get_text("text").insert_unicode(0, "after").unwrap();
        dst.commit_then_renew();
        assert_eq!(dst.state_frontiers(), dst.oplog_frontiers());
    }
}

/// A batch can unlock changes that were parked *before* it started. If the batch is
/// rolled back, those changes must be re-parked, or the document would silently drop
/// them and diverge once their deps finally arrive.
#[test]
fn failed_import_batch_reparks_prebatch_pending_changes() {
    for updates_only in [false, true] {
        let src = LoroDoc::new_auto_commit();
        src.set_peer_id(1).unwrap();
        src.get_map("map").insert("seed", "base").unwrap();
        let update_base = src
            .export(ExportMode::updates(&VersionVector::default()))
            .unwrap();
        let version_base = src.oplog_vv();
        src.get_map("map").insert("later", "value").unwrap();
        let update_later = src.export(ExportMode::updates(&version_base)).unwrap();

        let dst = LoroDoc::new();
        dst.import(&update_later).unwrap();
        assert_eq!(pending_len(&dst), 1);
        let vv_before = dst.oplog_vv();
        let frontiers_before = dst.oplog_frontiers();
        let state_before = dst.get_deep_value();

        // `update_base` unlocks the pre-batch pending change mid-batch; the malformed
        // blob then makes the closing reattach fail and rolls the whole batch back.
        let err = import_test_batch(
            &dst,
            &[update_base.clone(), malformed_binary_update(31)],
            updates_only,
        )
        .expect_err("the malformed blob must fail the whole batch");
        assert!(err.to_string().contains("list diff"), "{err:?}");
        assert!(!dst.is_detached());
        assert_eq!(
            pending_len(&dst),
            1,
            "the pending change the batch unlocked must be re-parked"
        );
        assert_doc_unchanged(&dst, &vv_before, &frontiers_before, &state_before);

        // Retrying without the bad blob applies base and unlocks the re-parked change.
        dst.import(&update_base).unwrap();
        assert_eq!(pending_len(&dst), 0);
        assert_eq!(dst.oplog_vv(), src.oplog_vv());
        assert_eq!(dst.get_deep_value(), src.get_deep_value());
    }
}

/// Changes are parked under the ID of the dep they are missing, so two peers waiting
/// on the same change share one pending slot. When a batch appends to a slot that
/// already held a pre-batch change, rollback has to trim its own entry off and keep
/// the older one — dropping the slot loses a change the document still needs, and
/// keeping both resurrects a change whose arena registrations were just truncated.
#[test]
fn failed_import_batch_trims_only_its_own_entry_from_a_shared_pending_slot() {
    for updates_only in [false, true] {
        let p1 = LoroDoc::new_auto_commit();
        p1.set_peer_id(1).unwrap();
        p1.get_map("map").insert("seed", "v").unwrap();
        p1.commit_then_renew();
        // Never shipped to `dst`, so everything below stays parked on it.
        let u_seed = p1
            .export(ExportMode::updates(&VersionVector::default()))
            .unwrap();
        let vv_seed = p1.oplog_vv();

        let waiter = |peer: u64, key: &str| {
            let d = LoroDoc::new_auto_commit();
            d.set_peer_id(peer).unwrap();
            d.import(&u_seed).unwrap();
            d.get_map("map").insert(key, "v").unwrap();
            d.commit_then_renew();
            d.export(ExportMode::updates(&vv_seed)).unwrap()
        };
        let u_p2 = waiter(2, "p2");
        let u_p3 = waiter(3, "p3");

        let dst = LoroDoc::new_auto_commit();
        dst.set_peer_id(9).unwrap();
        dst.import(&u_p2).unwrap();
        assert_eq!(pending_len(&dst), 1, "p2 waits on the seed change");
        let vv_before = dst.oplog_vv();
        let frontiers_before = dst.oplog_frontiers();
        let state_before = dst.get_deep_value();

        // `u_p3` parks in the same slot as `u_p2`; the malformed blob then fails the
        // closing reattach and rolls the whole batch back.
        let err = import_test_batch(
            &dst,
            &[u_p3.clone(), malformed_binary_update(41)],
            updates_only,
        )
        .expect_err("the malformed blob must fail the whole batch");
        assert!(err.to_string().contains("list diff"), "{err:?}");
        assert!(!dst.is_detached());
        assert_eq!(
            pending_len(&dst),
            1,
            "rollback must keep the pre-batch change and drop only the batch's own"
        );
        assert_doc_unchanged(&dst, &vv_before, &frontiers_before, &state_before);

        // The kept change is still the right one, and the doc converges on a retry.
        dst.import(&u_seed).unwrap();
        assert_eq!(pending_len(&dst), 0, "the seed unlocks the kept p2 change");
        dst.import(&u_p3).unwrap();

        let expected = LoroDoc::new_auto_commit();
        expected.set_peer_id(8).unwrap();
        expected.import(&u_seed).unwrap();
        expected.import(&u_p2).unwrap();
        expected.import(&u_p3).unwrap();
        assert_eq!(dst.oplog_vv(), expected.oplog_vv());
        assert_eq!(dst.get_deep_value(), expected.get_deep_value());
    }
}

/// One blob can both park a change and unlock it in the same import: B1 parks while
/// A1 is missing, B0 unlocks A1, and the cascade then applies B1. If that import's
/// state apply fails, rollback must re-park only what was pending *before* the
/// import — resurrecting the import's own parked changes would leave pending entries
/// whose arena registrations were just rolled back.
#[test]
fn failed_import_reparks_only_preexisting_pending_changes() {
    use loro_common::IdSpan;

    let a = LoroDoc::new_auto_commit();
    a.set_peer_id(1).unwrap();
    a.get_text("text").insert_unicode(0, "A0").unwrap();
    let u_a0 = a
        .export(ExportMode::updates(&VersionVector::default()))
        .unwrap();
    let vv_a0 = a.oplog_vv();
    let a_counter_after_a0 = *vv_a0.get(&1).unwrap();

    let b = LoroDoc::new_auto_commit();
    b.set_peer_id(2).unwrap();
    b.import(&u_a0).unwrap();
    b.get_text("text").insert_unicode(2, "B0").unwrap();
    let u_b0 = b.export(ExportMode::updates(&vv_a0)).unwrap();

    a.import(&u_b0).unwrap();
    a.get_text("text").insert_unicode(4, "A1").unwrap();
    let a_counter_after_a1 = *a.oplog_vv().get(&1).unwrap();
    // Peer-A ops only (A1), without B0 — pending on a doc that only has A0.
    let u_a1_only = a
        .export(ExportMode::updates_in_range(vec![IdSpan::new(
            1,
            a_counter_after_a0,
            a_counter_after_a1,
        )]))
        .unwrap();

    b.import(&u_a1_only).unwrap();
    b.get_text("text").insert_unicode(6, "B1").unwrap();
    // Single blob with B0 and B1; B1 depends on A1.
    let u_b0_b1 = b.export(ExportMode::updates(&vv_a0)).unwrap();

    let d = LoroDoc::new_auto_commit();
    d.set_peer_id(3).unwrap();
    d.import(&u_a0).unwrap();
    d.import(&u_a1_only).unwrap();
    assert_eq!(pending_len(&d), 1);
    let vv_before = d.oplog_vv();
    let frontiers_before = d.oplog_frontiers();
    let state_before = d.get_deep_value();

    fail_next_import_state_apply_for_test();
    let err = d.import(&u_b0_b1).unwrap_err();
    assert!(err.to_string().contains("state apply failpoint"), "{err:?}");
    assert_eq!(
        pending_len(&d),
        1,
        "only the pre-import pending change may be re-parked"
    );
    assert_doc_unchanged(&d, &vv_before, &frontiers_before, &state_before);

    // Retrying converges.
    d.import(&u_b0_b1).unwrap();
    assert_eq!(pending_len(&d), 0);
    assert_eq!(d.oplog_vv(), b.oplog_vv());
    assert_eq!(d.get_deep_value(), b.get_deep_value());
}

/// The same guarantee for a panic (malformed remote data reaching an `unreachable!`
/// in decode/apply is the shape that originally stranded the document). The blobs
/// imported before the panic must still be applied to `DocState`.
#[test]
fn import_batch_panic_leaves_doc_attached() {
    for updates_only in [false, true] {
        let (dst, blobs, expected) = doc_with_snapshot_and_pending_updates();
        assert!(blobs.len() > 2);

        crate::loro::panic_at_batch_import_blob_for_test(2);
        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = import_test_batch(&dst, &blobs, updates_only);
        }));
        assert!(panicked.is_err(), "the failpoint should have panicked");

        assert!(
            !dst.is_detached(),
            "import_batch must leave the doc attached"
        );
        assert!(!dst.oplog().lock().batch_importing);
        assert_eq!(dst.state_frontiers(), dst.oplog_frontiers());

        // The document still accepts the updates it missed.
        import_test_batch(&dst, &blobs, updates_only).unwrap();
        assert!(!dst.is_detached());
        assert_eq!(dst.oplog_vv(), expected.oplog_vv());
        assert_eq!(dst.get_deep_value(), expected.get_deep_value());
    }
}

/// A doc that was already detached must stay detached: the batch is only allowed to
/// restore the pre-batch mode, not to silently attach.
#[test]
fn import_batch_keeps_explicitly_detached_doc_detached() {
    for updates_only in [false, true] {
        let (dst, blobs, _) = doc_with_snapshot_and_pending_updates();
        dst.detach();
        let state_frontiers = dst.state_frontiers();

        import_test_batch(&dst, &blobs, updates_only).unwrap();
        assert!(dst.is_detached());
        assert_eq!(dst.state_frontiers(), state_frontiers);
        assert!(!dst.oplog().lock().batch_importing);
    }
}

#[test]
fn corrupt_snapshot_import_rolls_back_empty_doc() {
    let src = LoroDoc::new_auto_commit();
    src.set_peer_id(9).unwrap();
    src.get_text("text").insert_unicode(0, "snapshot").unwrap();
    src.get_list("list").push("value").unwrap();
    let snapshot = src.export(ExportMode::Snapshot).unwrap();
    let mut corrupt_snapshot = snapshot.clone();
    corrupt_snapshot_state_bytes(&mut corrupt_snapshot);

    let dst = LoroDoc::new();
    let vv_before_import = dst.oplog_vv();
    let frontiers_before_import = dst.oplog_frontiers();
    let state_before_import = dst.get_deep_value();
    let err = dst.import(&corrupt_snapshot).unwrap_err();
    assert!(
        err.to_string().contains("decode_snapshot")
            || err.to_string().contains("Decode")
            || err.to_string().contains("snapshot"),
        "unexpected error: {err:?}"
    );
    assert_eq!(dst.oplog_vv(), vv_before_import);
    assert_eq!(dst.oplog_frontiers(), frontiers_before_import);
    assert_eq!(dst.get_deep_value(), state_before_import);
    assert!(dst.oplog().lock().is_empty());

    dst.import(&snapshot).unwrap();
    assert_eq!(dst.get_deep_value(), src.get_deep_value());
}

/// Replace only a Graph wrapper while preserving valid SSTable and document checksums.
fn snapshot_with_invalid_graph_wrapper(
    snapshot: &[u8],
    graph: &loro_common::ContainerID,
    section_index: usize,
    wrong_kind: bool,
) -> Vec<u8> {
    use crate::utils::kv_wrapper::KvWrapper;
    use bytes::Bytes;
    use loro_common::{ContainerID, ContainerType};

    let mut remaining = &snapshot[22..];
    let mut sections = Vec::new();
    for _ in 0..3 {
        let length = u32::from_le_bytes(remaining[..4].try_into().unwrap()) as usize;
        sections.push(Bytes::copy_from_slice(&remaining[4..4 + length]));
        remaining = &remaining[4 + length..];
    }
    assert!(remaining.is_empty());
    let kv = KvWrapper::new_mem();
    if section_index == 1 && !sections[2].is_empty() {
        // The fixture exports at its latest shallow root. A complete overlay at
        // that same version also exercises decode_twice after a valid root decode.
        kv.import(sections[2].clone()).unwrap();
        kv.remove(b"fr");
    } else {
        kv.import(sections[section_index].clone()).unwrap();
    }
    let key = graph.to_bytes();
    assert!(kv.contains_key(&key));
    let wrapper = if wrong_kind {
        let map_key = ContainerID::new_root("map", ContainerType::Map).to_bytes();
        // A real, complete Map wrapper, not just a malformed Graph payload.
        kv.get(&map_key).unwrap()
    } else {
        Bytes::from_static(&[6]) // Graph kind, missing depth and parent.
    };
    kv.insert(&key, wrapper);
    sections[section_index] = kv.export();
    let mut result = snapshot[..22].to_vec();
    for bytes in sections {
        result.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        result.extend_from_slice(&bytes);
    }
    refresh_snapshot_checksum(&mut result);
    crate::encoding::parse_header_and_body(&result, true).unwrap();
    result
}

#[test]
fn graph_snapshot_invalid_wrappers_return_errors_and_leave_doc_usable() {
    use crate::{handler::GraphHandler, HandlerTrait};

    for nested in [false, true] {
        let source = LoroDoc::new_auto_commit();
        source.set_peer_id(1).unwrap();
        source.get_map("map").insert("keep", true).unwrap();
        let graph = if nested {
            source
                .get_map("map")
                .insert_container("graph", GraphHandler::new_detached())
                .unwrap()
        } else {
            source.get_graph("graph")
        };
        let node = graph.create_node().unwrap();
        graph
            .node_meta(node)
            .unwrap()
            .insert("title", "preserved")
            .unwrap();
        source.commit_then_renew();
        let frontiers = source.state_frontiers();
        for shallow in [false, true] {
            let snapshot = source
                .export(if shallow {
                    ExportMode::shallow_snapshot(&frontiers)
                } else {
                    ExportMode::Snapshot
                })
                .unwrap();
            for (section, wrong_kind) in [(1, false), (1, true), (2, false), (2, true)] {
                if section == 2 && !shallow {
                    continue;
                }
                let corrupted = snapshot_with_invalid_graph_wrapper(
                    &snapshot,
                    &graph.id(),
                    section,
                    wrong_kind,
                );
                let target = LoroDoc::new_auto_commit();
                target.set_peer_id(99).unwrap();
                let vv_before = target.oplog_vv();
                let frontiers_before = target.state_frontiers();
                let value_before = target.get_deep_value();
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    target.import(&corrupted)
                }));
                let error = result
                    .expect("invalid Graph wrappers must return Err, not panic")
                    .expect_err("invalid Graph wrapper was accepted");
                assert!(matches!(
                    error,
                    LoroError::DecodeError(_) | LoroError::DecodeDataCorruptionError
                ));
                assert_doc_unchanged(&target, &vv_before, &frontiers_before, &value_before);
                assert_eq!(target.state_frontiers(), frontiers_before);
                assert!(!target.is_detached());
                assert_eq!(pending_len(&target), 0);
                assert!(target.oplog().lock().is_empty());
                assert!(!target.oplog().lock().has_import_rollback());

                target.import(&snapshot).unwrap();
                assert_eq!(target.get_deep_value(), source.get_deep_value());
                target.get_graph(graph.id()).create_node().unwrap();
                target
                    .get_map("map")
                    .insert("after_rejection", true)
                    .unwrap();
                target.commit_then_renew();
                assert_eq!(target.state_frontiers(), target.oplog_frontiers());
                assert_eq!(target.get_graph(graph.id()).node_count(), 2);
            }
        }
    }
}

#[test]
fn snapshot_import_rejects_corrupt_inner_sstable_with_valid_envelope_checksum() {
    let src = LoroDoc::new_auto_commit();
    src.set_peer_id(9).unwrap();
    src.get_map("map").insert("key", "value").unwrap();
    let snapshot = src.export(ExportMode::Snapshot).unwrap();

    for section in [SnapshotKvSection::OpLog, SnapshotKvSection::State] {
        let mut corrupted = snapshot.clone();
        corrupt_snapshot_sstable_block(&mut corrupted, section);

        let dst = LoroDoc::new();
        let err = dst
            .import(&corrupted)
            .expect_err("invalid inner SSTable checksum must fail during import");
        assert!(
            err.to_string().contains("checksum") || err.to_string().contains("Checksum"),
            "unexpected {section:?} import error: {err:?}"
        );
        assert!(dst.oplog().lock().is_empty());
        assert!(dst
            .get_deep_value()
            .as_map()
            .is_some_and(|value| value.is_empty()));
    }
}

/// A change whose deps are before the shallow root is rejected AND dropped:
/// it must not linger in the pending store, where it could never be unlocked.
/// Locks in the "dropped, not pending" guarantee documented by
/// `loro/tests/shallow_snapshot_concurrency.rs`.
#[test]
fn outdated_update_on_shallow_doc_is_dropped_not_pending() {
    let a = LoroDoc::new_auto_commit();
    a.set_peer_id(1).unwrap();
    a.get_map("m").insert("a", 1).unwrap();
    a.commit_then_renew();
    let v_vv = a.oplog_vv();
    let v_frontiers = a.oplog_frontiers();
    a.get_map("m").insert("b", 2).unwrap();
    a.commit_then_renew();
    let f = a.oplog_frontiers();
    a.get_map("m").insert("c", 3).unwrap();
    a.commit_then_renew();

    // B is bootstrapped from the shallow snapshot at F.
    let b = LoroDoc::new();
    b.import(&a.export(ExportMode::shallow_snapshot(&f)).unwrap())
        .unwrap();
    assert_eq!(pending_len(&b), 0);

    // C holds full history up to V and edits on top of it, concurrent with F.
    let c = LoroDoc::new();
    c.import(&a.export(ExportMode::snapshot_at(&v_frontiers)).unwrap())
        .unwrap();
    c.set_peer_id(2).unwrap();
    c.get_map("m").insert("from_c", true).unwrap();
    c.commit_then_renew();
    let updates = c.export(ExportMode::updates(&v_vv)).unwrap();

    let err = b.import(&updates).unwrap_err();
    assert!(matches!(
        err,
        LoroError::ImportUpdatesThatDependsOnOutdatedVersion
    ));
    assert_eq!(
        pending_len(&b),
        0,
        "outdated changes must be dropped, not parked as pending"
    );
}

// Keep the established rollback, pending-journal, panic, and detach fixtures shared
// so changes to the execution kernel cannot silently weaken either entry point.
fn import_test_batch(
    doc: &LoroDoc,
    blobs: &[Vec<u8>],
    updates_only: bool,
) -> loro_common::LoroResult<crate::encoding::ImportStatus> {
    if updates_only {
        doc.import_updates_batch(&blobs.iter().map(Vec::as_slice).collect::<Vec<_>>())
    } else {
        doc.import_batch(blobs)
    }
}

#[test]
fn import_updates_batch_skips_metadata_decode_for_every_batch_size() {
    use crate::encoding::import_blob_meta_decode_count_for_test as count;
    let (_, blobs, _) = doc_with_snapshot_and_pending_updates();
    for size in [0, 1, blobs.len()] {
        let target = LoroDoc::new();
        let borrowed: Vec<_> = blobs[..size].iter().map(Vec::as_slice).collect();
        let before = count();
        target.import_updates_batch(&borrowed).unwrap();
        assert_eq!(
            count(),
            before,
            "updates-only path must not decode metadata"
        );
    }
    // Positive control: the same fixture really exercises the legacy metadata path.
    let before = count();
    LoroDoc::new().import_batch(&blobs).unwrap();
    assert_eq!(count() - before, blobs.len());
}

#[test]
fn import_updates_batch_checksum_and_body_errors_cleanup_and_allow_retry() {
    let source = LoroDoc::new_auto_commit();
    source.set_peer_id(17).unwrap();
    source.get_text("text").insert_unicode(0, "first").unwrap();
    let first = source.export(ExportMode::all_updates()).unwrap();
    let vv = source.oplog_vv();
    source.get_text("text").insert_unicode(5, "second").unwrap();
    let second = source.export(ExportMode::updates(&vv)).unwrap();
    let mut bad_checksum = second.clone();
    bad_checksum[16] ^= 1;
    let mut bad_body = second[..22].to_vec();
    bad_body.extend_from_slice(&[0x02, 0x01]); // Valid envelope, truncated change block.
    refresh_snapshot_checksum(&mut bad_body);
    for (bad, checksum_error) in [(bad_checksum, true), (bad_body, false)] {
        for multi in [false, true] {
            let target = LoroDoc::new_auto_commit();
            let blobs: Vec<&[u8]> = if multi {
                vec![&first, &bad, &second]
            } else {
                vec![&bad]
            };
            let err = target.import_updates_batch(&blobs).unwrap_err();
            if checksum_error {
                assert!(matches!(err, LoroError::DecodeChecksumMismatchError));
            } else {
                assert!(matches!(err, LoroError::DecodeError(_)), "{err:?}");
            }
            assert!(!target.is_detached());
            assert!(!target.oplog().lock().batch_importing);
            assert!(!target.oplog().lock().has_import_rollback());
            assert_eq!(target.state_frontiers(), target.oplog_frontiers());
            if multi {
                // Decode errors are not ACID: good blobs before AND after the error
                // can survive. Do not require rollback of the whole batch here.
                assert_eq!(target.oplog_vv(), source.oplog_vv());
                assert_eq!(target.get_deep_value(), source.get_deep_value());
            }
            target.import_updates_batch(&[&second, &first]).unwrap();
            assert_eq!(target.oplog_vv(), source.oplog_vv());
            assert_eq!(target.get_deep_value(), source.get_deep_value());
            target.get_text("text").insert_unicode(0, "local").unwrap();
            target.commit_then_renew();
            assert_eq!(target.state_frontiers(), target.oplog_frontiers());
        }
    }
}

#[test]
fn import_updates_batch_single_final_checkout_failure_restores_guard_state() {
    let target = LoroDoc::new_auto_commit();
    target.set_peer_id(10).unwrap();
    target.get_text("text").insert_unicode(0, "before").unwrap();
    target.commit_then_renew();
    let vv = target.oplog_vv();
    let frontiers = target.oplog_frontiers();
    let value = target.get_deep_value();
    let bad = malformed_binary_update(11);
    let err = target.import_updates_batch(&[&bad]).unwrap_err();
    assert!(err.to_string().contains("list diff"), "{err:?}");
    assert!(!target.is_detached());
    assert!(!target.oplog().lock().batch_importing);
    assert!(!target.oplog().lock().has_import_rollback());
    assert_doc_unchanged(&target, &vv, &frontiers, &value);
    assert!(target.import_updates_batch(&[]).unwrap().pending.is_none());
    target.get_text("text").insert_unicode(0, "after").unwrap();
    target.commit_then_renew();
    assert_eq!(target.state_frontiers(), target.oplog_frontiers());
}

fn richtext_diff_counts() -> (u64, u64) {
    use crate::diff_calc::{CRDT_RICHTEXT_DIFF_COUNT, LINEAR_RICHTEXT_DIFF_COUNT};
    (
        LINEAR_RICHTEXT_DIFF_COUNT.with(|count| count.get()),
        CRDT_RICHTEXT_DIFF_COUNT.with(|count| count.get()),
    )
}

/// Reproduces the history materialization shape that regressed when a consumer
/// switched from sequential imports to batching: three seed appends followed by
/// tiny tail replacements. Fixture construction is outside the counted section.
fn large_text_tail_edits() -> (LoroDoc, Vec<Vec<u8>>, Frontiers) {
    let source = LoroDoc::new_auto_commit();
    source.set_peer_id(42).unwrap();
    let text = source.get_text("text");
    let mut vv = VersionVector::default();
    let mut blobs = Vec::new();
    let mut first = Frontiers::default();
    let mut len = 0;
    for size in [24 * 1024, 48 * 1024, 64 * 1024] {
        text.insert_unicode(len, &"s".repeat(size - len)).unwrap();
        blobs.push(source.export(ExportMode::updates(&vv)).unwrap());
        vv = source.oplog_vv();
        if len == 0 {
            first = source.oplog_frontiers();
        }
        len = size;
    }
    for i in 0..12 {
        text.delete_unicode(len - 1, 1).unwrap();
        text.insert_unicode(len - 1, if i % 2 == 0 { "a" } else { "b" })
            .unwrap();
        blobs.push(source.export(ExportMode::updates(&vv)).unwrap());
        vv = source.oplog_vv();
    }
    (source, blobs, first)
}

#[test]
fn batch_large_linear_text_uses_linear_diff_and_keeps_history_checkout_usable() {
    let (source, blobs, first) = large_text_tail_edits();
    for updates_only in [false, true] {
        for reversed in [false, true] {
            for seeded in [false, true] {
                let target = LoroDoc::new_auto_commit();
                if seeded {
                    target.import(&blobs[0]).unwrap();
                    // Populate a persistent tracker before the one-shot batch. Its
                    // subsequent reuse must remain correct after the state advances.
                    target.checkout(&Frontiers::default()).unwrap();
                    target.checkout_to_latest();
                }
                let mut remaining = blobs[usize::from(seeded)..].to_vec();
                if reversed {
                    remaining.reverse();
                }
                let before = richtext_diff_counts();
                let status = import_test_batch(&target, &remaining, updates_only).unwrap();
                let after = richtext_diff_counts();
                assert_eq!(after.0 - before.0, 1, "one linear richtext diff per batch");
                assert_eq!(
                    after.1 - before.1,
                    0,
                    "linear history needs no CRDT tracker"
                );
                assert!(status.pending.is_none());
                assert_eq!(target.oplog_vv(), source.oplog_vv());
                assert_eq!(target.get_deep_value(), source.get_deep_value());
                assert_eq!(target.state_frontiers(), target.oplog_frontiers());
                assert!(!target.is_detached());

                let before_checkout = richtext_diff_counts();
                target.checkout(&first).unwrap();
                assert_eq!(target.get_text("text").to_string(), "s".repeat(24 * 1024));
                target.checkout_to_latest();
                assert!(
                    richtext_diff_counts().1 > before_checkout.1,
                    "real history checkout still exercises the persistent tracker"
                );
                assert_eq!(target.get_deep_value(), source.get_deep_value());
                target.get_text("text").insert_unicode(0, "local").unwrap();
                target.commit_then_renew();
                assert_eq!(target.state_frontiers(), target.oplog_frontiers());
            }
        }
    }
}

#[test]
fn batch_concurrent_text_uses_crdt_diff_and_matches_sequential_imports() {
    let base = LoroDoc::new_auto_commit();
    base.set_peer_id(1).unwrap();
    base.get_text("text").insert_unicode(0, "abcdef").unwrap();
    let seed = base.export(ExportMode::all_updates()).unwrap();
    let vv = base.oplog_vv();
    let left = base.fork();
    left.set_peer_id(2).unwrap();
    left.get_text("text").delete_unicode(1, 2).unwrap();
    left.get_text("text").insert_unicode(1, "L").unwrap();
    let left_update = left.export(ExportMode::updates(&vv)).unwrap();
    let right = base.fork();
    right.set_peer_id(3).unwrap();
    right.get_text("text").insert_unicode(2, "R").unwrap();
    let right_update = right.export(ExportMode::updates(&vv)).unwrap();
    let expected = LoroDoc::new_auto_commit();
    for blob in [&seed, &left_update, &right_update] {
        expected.import(blob).unwrap();
    }
    for updates_only in [false, true] {
        for seeded in [false, true] {
            let target = LoroDoc::new_auto_commit();
            let mut blobs = vec![right_update.clone(), left_update.clone()];
            if seeded {
                target.import(&seed).unwrap();
            } else {
                blobs.push(seed.clone());
            }
            let before = richtext_diff_counts();
            let status = import_test_batch(&target, &blobs, updates_only).unwrap();
            let after = richtext_diff_counts();
            assert_eq!(after.0, before.0, "concurrency cannot be treated as linear");
            assert!(after.1 > before.1);
            assert!(status.pending.is_none());
            assert_eq!(target.get_deep_value(), expected.get_deep_value());
            assert_eq!(target.oplog_vv(), expected.oplog_vv());
            assert!(!target.is_detached());
        }
    }
}
