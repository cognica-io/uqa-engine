//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Joint backup cuts preserve autonomous allocations while short physical reads release writers.

use super::*;
use crate::mvcc::backup_metadata::{BIRTHS, EPOCH, HISTORY};
use uqa_storage::mvcc::{CommittedBackupSnapshot, IdentifierRequest, MetadataEpoch, RecordWrite};

fn memory_store() -> RedbRecordStore {
    RedbRecordStore::new(Arc::new(
        Database::builder()
            .create_with_backend(InMemoryBackend::new())
            .unwrap(),
    ))
    .unwrap()
}

fn observe(store: &RedbRecordStore, namespace: &[u8], value: u64, control: &StorageReadControl) {
    store
        .allocate_identifiers(namespace, IdentifierRequest::Observe(value), control)
        .unwrap();
}

fn capture(
    store: &RedbRecordStore,
    control: &StorageReadControl,
) -> Arc<dyn CommittedBackupSnapshot> {
    store.capture_backup_snapshot(control).unwrap().unwrap()
}

fn write(
    store: &RedbRecordStore,
    expected: Option<CommitSequence>,
    value: &[u8],
    control: &StorageReadControl,
) -> CommitSequence {
    let prepared = PreparedRecordCommit::new(
        &[RecordWrite {
            key: b"row",
            expected,
            value: Some(value),
        }],
        control,
    )
    .unwrap();
    let transaction = store.allocate_transaction(control).unwrap();
    store
        .commit(transaction, &prepared, control)
        .unwrap()
        .sequence
}

fn watermarks(
    snapshot: &dyn CommittedBackupSnapshot,
    control: &StorageReadControl,
) -> Vec<(Vec<u8>, u64)> {
    let mut result = Vec::new();
    snapshot
        .visit_identifier_watermarks(None, usize::MAX, control, &mut |key, value| {
            if let Some(value) = value {
                result.push((key.to_vec(), value));
            }
            Ok(true)
        })
        .unwrap();
    result
}

fn history_len(store: &RedbRecordStore) -> u64 {
    let read = store.database.begin_read().unwrap();
    read.open_table(HISTORY).unwrap().len().unwrap()
}

#[test]
fn backup_cuts_retain_records_and_autonomous_watermarks_independently() {
    let store = memory_store();
    let control = StorageReadControl::with_limit(64 << 10);
    observe(&store, b"a", 10, &control);
    let first_sequence = write(&store, None, b"first", &control);
    let first = capture(&store, &control);
    assert_eq!(first.metadata_epoch(), MetadataEpoch::INITIAL);
    assert_eq!(first.records().sequence(), first_sequence);
    assert_eq!(first.database_id(), store.identity);
    observe(&store, b"a", 20, &control);
    let second_sequence = write(&store, Some(first_sequence), b"second", &control);
    let second = capture(&store, &control);
    assert_eq!(second.metadata_epoch(), MetadataEpoch::from_u64(1));
    assert_eq!(second.records().sequence(), second_sequence);
    observe(&store, b"a", 30, &control);
    observe(&store, b"b", 40, &control);
    assert_eq!(
        store.snapshot(&control).unwrap().sequence(),
        second_sequence
    );
    assert_eq!(history_len(&store), 2);
    store.reclaim_versions(&control).unwrap();
    assert_eq!(history_len(&store), 2);
    assert_eq!(
        watermarks(first.as_ref(), &control),
        vec![(b"a".to_vec(), 10)]
    );
    assert_eq!(
        watermarks(second.as_ref(), &control),
        vec![(b"a".to_vec(), 20)]
    );
    assert_eq!(
        &***first
            .records()
            .get(b"row", &control)
            .unwrap()
            .unwrap()
            .value()
            .unwrap(),
        b"first"
    );
    assert_eq!(
        &***second
            .records()
            .get(b"row", &control)
            .unwrap()
            .unwrap()
            .value()
            .unwrap(),
        b"second"
    );
    drop(first);
    store.reclaim_versions(&control).unwrap();
    assert_eq!(history_len(&store), 1);
    assert_eq!(
        watermarks(second.as_ref(), &control),
        vec![(b"a".to_vec(), 20)]
    );
    drop(second);
    assert_eq!(control.memory().used(), 0);
    store.reclaim_versions(&control).unwrap();
    assert_eq!(history_len(&store), 0);
    let latest = capture(&store, &control);
    drop(store);
    assert_eq!(
        watermarks(latest.as_ref(), &control),
        vec![(b"a".to_vec(), 30), (b"b".to_vec(), 40)]
    );
    drop(latest);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn watermark_history_coalesces_between_captures_and_scans_bound_absent_positions() {
    let store = memory_store();
    let control = StorageReadControl::with_limit(64 << 10);
    observe(&store, b"z", 1, &control);
    let snapshot = capture(&store, &control);
    for value in 2..=100 {
        observe(&store, b"z", value, &control);
    }
    for key in 0..8_u8 {
        observe(&store, &[b'a', key], u64::from(key), &control);
    }
    assert_eq!(history_len(&store), 1);
    let mut after = None::<Vec<u8>>;
    let mut visited = 0;
    let mut visible = Vec::new();
    loop {
        let mut page = Vec::new();
        snapshot
            .visit_identifier_watermarks(after.as_deref(), 2, &control, &mut |key, value| {
                page.push((key.to_vec(), value));
                Ok(true)
            })
            .unwrap();
        assert!(page.len() <= 2);
        if page.is_empty() {
            break;
        }
        visited += page.len();
        after = Some(page.last().unwrap().0.clone());
        visible.extend(
            page.into_iter()
                .filter_map(|(key, value)| value.map(|value| (key, value))),
        );
        // This physical writer completes while the logical backup view remains retained between pages.
        std::thread::scope(|scope| {
            scope
                .spawn(|| observe(&store, b"z", 101, &control))
                .join()
                .unwrap();
        });
    }
    assert_eq!(visited, 9);
    assert_eq!(visible, vec![(b"z".to_vec(), 1)]);
    assert_eq!(snapshot.records().sequence(), CommitSequence::INITIAL);
    assert_eq!(history_len(&store), 1);
}

#[test]
fn backup_metadata_rejects_missing_anchors_heads_and_unsealed_epochs() {
    for defect in 0..4 {
        let store = memory_store();
        let control = StorageReadControl::with_limit(64 << 10);
        observe(&store, b"a", 10, &control);
        let snapshot = capture(&store, &control);
        observe(&store, b"a", 20, &control);
        let transaction = physical_writer(&store.database).unwrap();
        match defect {
            0 => {
                transaction
                    .open_table(HISTORY)
                    .unwrap()
                    .remove((b"a".as_slice(), 0))
                    .unwrap();
            }
            1 => {
                transaction
                    .open_table(BIRTHS)
                    .unwrap()
                    .remove(b"a".as_slice())
                    .unwrap();
            }
            2 => {
                transaction
                    .open_table(BIRTHS)
                    .unwrap()
                    .insert(b"a".as_slice(), (2, 1))
                    .unwrap();
            }
            _ => {
                transaction
                    .open_table(METADATA)
                    .unwrap()
                    .insert(EPOCH, 0_u64.to_be_bytes().as_slice())
                    .unwrap();
            }
        }
        transaction.commit().unwrap();
        let mut observed = false;
        let result = snapshot.visit_identifier_watermarks(None, 2, &control, &mut |_, _| {
            observed = true;
            Ok(true)
        });
        assert!(matches!(result, Err(VersionError::InvalidEncoding(_))));
        assert!(!observed);
    }
}

#[test]
fn cancelled_and_inadmissible_backup_capture_releases_all_retention() {
    let store = memory_store();
    let cancelled = StorageReadControl::with_limit(64 << 10);
    cancelled.cancellation().cancel();
    assert!(matches!(
        store.capture_backup_snapshot(&cancelled),
        Err(VersionError::Cancelled(_))
    ));
    let zero = StorageReadControl::with_limit(0);
    assert!(store.capture_backup_snapshot(&zero).is_err());
    assert_eq!(zero.memory().used(), 0);
    let control = StorageReadControl::with_limit(64 << 10);
    assert_eq!(store.snapshots.reclaim(&control, Ok).unwrap(), None);
    assert_eq!(
        store.metadata_snapshots.reclaim(&control, Ok).unwrap(),
        None
    );
    let snapshot = capture(&store, &control);
    assert_eq!(snapshot.records().sequence(), CommitSequence::INITIAL);
    assert!(watermarks(snapshot.as_ref(), &control).is_empty());
    drop(snapshot);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn metadata_retention_is_shared_by_live_adapters_and_released_by_its_own_allowance() {
    let first = memory_store();
    let second = RedbRecordStore::new(Arc::clone(&first.database)).unwrap();
    let first_control = StorageReadControl::with_limit(64 << 10);
    let second_control = StorageReadControl::with_limit(64 << 10);
    observe(&first, b"a", 10, &first_control);
    let first_cut = capture(&first, &first_control);
    observe(&second, b"a", 20, &second_control);
    let second_cut = capture(&second, &second_control);
    observe(&first, b"a", 30, &first_control);
    second.reclaim_versions(&second_control).unwrap();
    assert_eq!(history_len(&second), 2);
    assert_eq!(
        watermarks(first_cut.as_ref(), &first_control),
        vec![(b"a".to_vec(), 10)]
    );
    drop((first_cut, first));
    assert_eq!(first_control.memory().used(), 0);
    assert!(second_control.memory().used() > 0);
    second.reclaim_versions(&second_control).unwrap();
    assert_eq!(history_len(&second), 1);
    assert_eq!(
        watermarks(second_cut.as_ref(), &second_control),
        vec![(b"a".to_vec(), 20)]
    );
    drop(second_cut);
    assert_eq!(second_control.memory().used(), 0);
    second.reclaim_versions(&second_control).unwrap();
    assert_eq!(history_len(&second), 0);
}

#[test]
fn backup_metadata_windows_release_after_stops_errors_and_cancellation() {
    let store = memory_store();
    let control = StorageReadControl::with_limit(64 << 10);
    for key in [b"a", b"b", b"c"] {
        observe(&store, key, 10, &control);
    }
    let snapshot = capture(&store, &control);
    snapshot
        .visit_identifier_watermarks(None, 0, &control, &mut |_, _| {
            panic!("a zero-sized window must not visit a position");
        })
        .unwrap();
    let mut visited = 0;
    snapshot
        .visit_identifier_watermarks(None, 3, &control, &mut |_, _| {
            visited += 1;
            Ok(false)
        })
        .unwrap();
    assert_eq!(visited, 1);
    assert!(matches!(
        snapshot.visit_identifier_watermarks(None, 3, &control, &mut |_, _| {
            Err(VersionError::InvalidEncoding("injected consumer failure"))
        }),
        Err(VersionError::InvalidEncoding("injected consumer failure"))
    ));
    let cancelled = StorageReadControl::with_limit(64 << 10);
    visited = 0;
    assert!(matches!(
        snapshot.visit_identifier_watermarks(None, 3, &cancelled, &mut |_, _| {
            visited += 1;
            cancelled.cancellation().cancel();
            Ok(true)
        }),
        Err(VersionError::Cancelled(_))
    ));
    assert_eq!(visited, 1);
    observe(&store, b"a", 20, &control);
    assert_eq!(
        watermarks(snapshot.as_ref(), &control),
        vec![
            (b"a".to_vec(), 10),
            (b"b".to_vec(), 10),
            (b"c".to_vec(), 10)
        ]
    );
}

#[test]
fn cold_upgrade_preserves_allocator_namespace_and_exhausted_metadata_epoch_rejects_capture() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("backup-metadata.redb");
    let control = StorageReadControl::with_limit(64 << 10);
    {
        let store = RedbRecordStore::new(Arc::new(Database::create(&path).unwrap())).unwrap();
        observe(&store, b"a", u64::MAX, &control);
        let transaction = physical_writer(&store.database).unwrap();
        transaction
            .open_table(METADATA)
            .unwrap()
            .insert("format", 63_u64.to_be_bytes().as_slice())
            .unwrap();
        remove_backup_metadata_for_predecessor(&transaction);
        transaction.commit().unwrap();
    }
    {
        let store = RedbRecordStore::new(Arc::new(Database::open(&path).unwrap())).unwrap();
        let cut = capture(&store, &control);
        assert_eq!(
            watermarks(cut.as_ref(), &control),
            vec![(b"a".to_vec(), u64::MAX)]
        );
        drop(cut);
    }
    let store = RedbRecordStore::new(Arc::new(Database::open(&path).unwrap())).unwrap();
    let cut = capture(&store, &control);
    let transaction = physical_writer(&store.database).unwrap();
    transaction
        .open_table(METADATA)
        .unwrap()
        .insert(EPOCH, u64::MAX.to_be_bytes().as_slice())
        .unwrap();
    transaction.commit().unwrap();
    assert!(matches!(
        store.capture_backup_snapshot(&control),
        Err(VersionError::InvalidEncoding("metadata epoch exhausted"))
    ));
    assert_eq!(
        watermarks(cut.as_ref(), &control),
        vec![(b"a".to_vec(), u64::MAX)]
    );
    observe(&store, b"b", 1, &control);
    assert_eq!(
        store.snapshot(&control).unwrap().sequence(),
        CommitSequence::INITIAL
    );
    assert_eq!(
        watermarks(cut.as_ref(), &control),
        vec![(b"a".to_vec(), u64::MAX)]
    );
    drop(cut);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn failed_capture_and_allocator_sync_recover_complete_metadata_states() {
    for fail_capture in [false, true] {
        let backend = FaultBackend::default();
        let control = StorageReadControl::with_limit(64 << 10);
        {
            let store = RedbRecordStore::new(Arc::new(
                Database::builder()
                    .create_with_backend(backend.clone())
                    .unwrap(),
            ))
            .unwrap();
            observe(&store, b"a", 10, &control);
            let cut = capture(&store, &control);
            backend.fail_sync.store(true, Ordering::Relaxed);
            if fail_capture {
                assert!(store.capture_backup_snapshot(&control).is_err());
            } else {
                assert!(store
                    .allocate_identifiers(b"a", IdentifierRequest::Observe(20), &control)
                    .is_err());
            }
            assert!(!backend.fail_sync.load(Ordering::Relaxed));
            drop(cut);
        }
        assert_eq!(control.memory().used(), 0);
        let store = RedbRecordStore::new(Arc::new(
            Database::builder().create_with_backend(backend).unwrap(),
        ))
        .unwrap();
        let observed = store.identifier_watermark(b"a", &control).unwrap().unwrap();
        assert!(observed == 10 || (!fail_capture && observed == 20));
        let cut = capture(&store, &control);
        observe(&store, b"a", 30, &control);
        store.reclaim_versions(&control).unwrap();
        assert_eq!(
            watermarks(cut.as_ref(), &control),
            vec![(b"a".to_vec(), observed)]
        );
        assert_eq!(cut.records().sequence(), CommitSequence::INITIAL);
        drop(cut);
        store.reclaim_versions(&control).unwrap();
        assert_eq!(history_len(&store), 0);
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn large_metadata_cut_streams_under_a_fixed_retention_allowance() {
    let store = memory_store();
    let transaction = physical_writer(&store.database).unwrap();
    {
        let mut identifiers = transaction
            .open_table(crate::mvcc::identifiers::TABLE)
            .unwrap();
        let mut births = transaction.open_table(BIRTHS).unwrap();
        for value in 0_u64..4096 {
            let key = value.to_be_bytes();
            identifiers.insert(key.as_slice(), key.as_slice()).unwrap();
            births.insert(key.as_slice(), (0, 0)).unwrap();
        }
    }
    transaction.commit().unwrap();
    let control = StorageReadControl::with_limit(512);
    let cut = capture(&store, &control);
    let retained = control.memory().used();
    let mut last = None::<[u8; 8]>;
    let mut count = 0_u64;
    loop {
        let mut end = last;
        let mut page_count = 0;
        cut.visit_identifier_watermarks(
            last.as_ref().map(<[u8; 8]>::as_slice),
            32,
            &control,
            &mut |key, value| {
                assert_eq!(key, count.to_be_bytes());
                assert_eq!(value, Some(count));
                end = Some(key.try_into().unwrap());
                page_count += 1;
                count += 1;
                Ok(true)
            },
        )
        .unwrap();
        assert!(page_count <= 32);
        assert_eq!(control.memory().used(), retained);
        if page_count == 0 {
            break;
        }
        last = end;
    }
    assert_eq!(count, 4096);
    assert_eq!(cut.records().sequence(), CommitSequence::INITIAL);
    drop(cut);
    assert_eq!(control.memory().used(), 0);
}
