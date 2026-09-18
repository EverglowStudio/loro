use loro::{ExportMode, IdSpan, LoroDoc, LoroError, VersionRange, VersionVector};

fn chain(peer: u64, root: &str, count: usize) -> (LoroDoc, Vec<Vec<u8>>) {
    let doc = LoroDoc::new();
    doc.set_peer_id(peer).unwrap();
    let mut updates = Vec::new();
    let mut vv = VersionVector::default();
    for i in 0..count {
        doc.get_text(root).insert(i, "x").unwrap();
        updates.push(doc.export(ExportMode::updates(&vv)).unwrap());
        vv = doc.oplog_vv();
    }
    (doc, updates)
}

fn pending(peer: u64, start: i32, end: i32) -> Option<VersionRange> {
    let mut range = VersionRange::default();
    range.insert(peer, start, end);
    Some(range)
}

#[test]
fn ordered_reversed_duplicate_and_overlapping_updates_match_legacy_batch() {
    let (source, updates) = chain(1, "body", 4);
    let overlap = source
        .export(ExportMode::updates_in_range(vec![IdSpan::new(1, 1, 4)]))
        .unwrap();
    for order in [vec![0, 1, 2, 3], vec![3, 2, 1, 0], vec![3, 1, 3, 0, 2, 0]] {
        let mut blobs: Vec<_> = order.iter().map(|&i| updates[i].clone()).collect();
        blobs.insert(0, overlap.clone());
        let borrowed: Vec<_> = blobs.iter().map(Vec::as_slice).collect();
        let old = LoroDoc::new();
        let new = LoroDoc::new();
        let old_status = old.import_batch(&blobs).unwrap();
        let new_status = new.import_updates_batch(&borrowed).unwrap();
        assert_eq!(new_status, old_status);
        assert!(new_status.pending.is_none());
        assert_eq!(new.oplog_vv(), source.oplog_vv());
        assert_eq!(new.oplog_vv(), old.oplog_vv());
        assert_eq!(new.get_deep_value(), source.get_deep_value());
        assert_eq!(new.get_deep_value(), old.get_deep_value());
        assert_eq!(new.state_frontiers(), new.oplog_frontiers());
    }
}

#[test]
fn every_batch_size_reports_preexisting_unrelated_pending_and_unlocks_it() {
    let (unrelated, waiting) = chain(91, "unrelated", 2);
    let (_, updates) = chain(1, "body", 2);
    for size in [0, 1, 2] {
        let target = LoroDoc::new();
        target.import(&waiting[1]).unwrap();
        let borrowed: Vec<_> = updates[..size].iter().map(Vec::as_slice).collect();
        let status = target.import_updates_batch(&borrowed).unwrap();
        assert_eq!(status.pending, pending(91, 1, 2));
        assert_eq!(
            target.import_updates_batch(&[]).unwrap().pending,
            status.pending
        );
        let expected = LoroDoc::new();
        for update in &updates[..size] {
            expected.import(update).unwrap();
        }
        assert_eq!(target.oplog_vv(), expected.oplog_vv());
        // Accessing a root from a pending op can register an empty container; compare
        // the relevant visible body rather than requiring absent roots to match.
        assert_eq!(
            target.get_text("body").to_string(),
            expected.get_text("body").to_string()
        );
        let unlocked = target.import_updates_batch(&[&waiting[0]]).unwrap();
        assert!(unlocked.pending.is_none());
        assert_eq!(unlocked.success.get(&91), Some(&(0, 2)));
        expected
            .import(&unrelated.export(ExportMode::all_updates()).unwrap())
            .unwrap();
        assert_eq!(target.oplog_vv(), expected.oplog_vv());
        assert_eq!(target.get_deep_value(), expected.get_deep_value());
    }
}

#[test]
fn missing_dependencies_and_overlaps_report_only_the_remaining_operations() {
    let (source, updates) = chain(1, "body", 4);
    let overlap = source
        .export(ExportMode::updates_in_range(vec![IdSpan::new(1, 1, 4)]))
        .unwrap();
    let target = LoroDoc::new();
    let status = target
        .import_updates_batch(&[&updates[3], &overlap, &updates[2]])
        .unwrap();
    assert!(status.success.is_empty());
    assert_eq!(status.pending, pending(1, 1, 4));
    let status = target.import_updates_batch(&[&updates[0]]).unwrap();
    assert!(status.pending.is_none());
    assert_eq!(status.success.get(&1), Some(&(0, 4)));
    assert_eq!(target.oplog_vv(), source.oplog_vv());
    assert_eq!(target.get_deep_value(), source.get_deep_value());
    assert!(target
        .import_updates_batch(&[&overlap])
        .unwrap()
        .pending
        .is_none());
    assert!(target.import_updates_batch(&[]).unwrap().pending.is_none());
}

#[test]
fn rejects_snapshot_shallow_legacy_and_invalid_headers_before_importing() {
    let (source, updates) = chain(1, "body", 2);
    let snapshot = source.export(ExportMode::Snapshot).unwrap();
    let shallow = source
        .export(ExportMode::shallow_snapshot(&source.oplog_frontiers()))
        .unwrap();
    for blob in [snapshot, shallow] {
        let target = LoroDoc::new();
        let err = target
            .import_updates_batch(&[&updates[0], &blob])
            .unwrap_err();
        assert!(matches!(err, LoroError::ImportUnsupportedEncodingMode));
        assert_eq!(target.oplog_vv(), VersionVector::default());
        assert!(!target.is_detached());
    }
    for mode in [0u16, 1, 2, 255] {
        let mut invalid = updates[0].clone();
        invalid[20..22].copy_from_slice(&mode.to_be_bytes());
        let target = LoroDoc::new();
        assert!(target
            .import_updates_batch(&[&updates[0], &invalid])
            .is_err());
        assert_eq!(target.oplog_vv(), VersionVector::default());
    }
    for invalid in [&b"loro"[..], &b"not a valid header with enough bytes"[..]] {
        let target = LoroDoc::new();
        assert!(target
            .import_updates_batch(&[&updates[0], invalid])
            .is_err());
        assert_eq!(target.oplog_vv(), VersionVector::default());
    }
}

#[test]
fn legacy_empty_single_and_mixed_snapshot_batch_keep_their_contracts() {
    let (source, updates) = chain(1, "body", 2);
    let (_, waiting) = chain(91, "unrelated", 2);
    let target = LoroDoc::new();
    target.import(&waiting[1]).unwrap();
    assert_eq!(target.import_batch(&[]).unwrap(), Default::default());
    assert!(target
        .import_batch(&updates[..1])
        .unwrap()
        .pending
        .is_none());
    assert_eq!(
        target.import_updates_batch(&[]).unwrap().pending,
        pending(91, 1, 2)
    );

    let snapshot = source.export(ExportMode::Snapshot).unwrap();
    let mixed = LoroDoc::new();
    let status = mixed
        .import_batch(&[updates[1].clone(), snapshot, updates[0].clone()])
        .unwrap();
    assert!(status.pending.is_none());
    assert_eq!(mixed.oplog_vv(), source.oplog_vv());
    assert_eq!(mixed.get_deep_value(), source.get_deep_value());
}

#[test]
fn batch_linear_richtext_preserves_styles_and_one_checkout_notification() {
    use loro::EventTriggerKind;
    use std::sync::{Arc, Mutex};

    let source = LoroDoc::new();
    source.set_peer_id(1).unwrap();
    let text = source.get_text("body");
    text.insert(0, "hello world").unwrap();
    let first = source.export(ExportMode::all_updates()).unwrap();
    let vv = source.oplog_vv();
    text.mark(0..5, "bold", true).unwrap();
    text.delete(10, 1).unwrap();
    text.insert(10, "!").unwrap();
    let second = source.export(ExportMode::updates(&vv)).unwrap();
    let target = LoroDoc::new();
    let events = Arc::new(Mutex::new(Vec::new()));
    let captured = events.clone();
    let _subscription = target.subscribe_root(Arc::new(move |event| {
        captured
            .lock()
            .unwrap()
            .push((event.triggered_by, event.origin.to_owned()));
    }));
    let status = target.import_updates_batch(&[&second, &first]).unwrap();
    assert!(status.pending.is_none());
    assert_eq!(target.oplog_vv(), source.oplog_vv());
    assert_eq!(target.get_deep_value(), source.get_deep_value());
    assert_eq!(target.get_text("body").to_delta(), text.to_delta());
    assert_eq!(
        events.lock().unwrap().as_slice(),
        &[(EventTriggerKind::Checkout, "checkout".to_owned())]
    );
    target.import_updates_batch(&[&first, &second]).unwrap();
    target.import_updates_batch(&[]).unwrap();
    assert_eq!(
        events.lock().unwrap().len(),
        1,
        "duplicates and empty input emit no new state event"
    );
}

#[test]
fn batch_on_shallow_text_preserves_the_existing_import_path() {
    let source = LoroDoc::new();
    source.set_peer_id(1).unwrap();
    source.get_text("body").insert(0, "seed text").unwrap();
    source.commit();
    let shallow = source
        .export(ExportMode::shallow_snapshot(&source.oplog_frontiers()))
        .unwrap();
    let mut vv = source.oplog_vv();
    let mut updates = Vec::new();
    for suffix in ["a", "b", "c"] {
        let text = source.get_text("body");
        text.delete(text.len_unicode() - 1, 1).unwrap();
        text.insert(text.len_unicode(), suffix).unwrap();
        updates.push(source.export(ExportMode::updates(&vv)).unwrap());
        vv = source.oplog_vv();
    }
    let expected = LoroDoc::new();
    expected.import(&shallow).unwrap();
    for update in &updates {
        expected.import(update).unwrap();
    }
    let target = LoroDoc::new();
    target.import(&shallow).unwrap();
    let borrowed: Vec<_> = updates.iter().rev().map(Vec::as_slice).collect();
    let status = target.import_updates_batch(&borrowed).unwrap();
    assert!(status.pending.is_none());
    assert_eq!(target.oplog_vv(), expected.oplog_vv());
    assert_eq!(target.get_deep_value(), expected.get_deep_value());
    assert_eq!(target.state_frontiers(), target.oplog_frontiers());
}
