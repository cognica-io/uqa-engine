//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Backup cuts keep autonomous metadata and record visibility at one durable boundary.

use super::*;

mod positions;
use uqa_storage::mvcc::{CommittedBackupSnapshot, IdentifierRequest, MetadataEpoch};

mod admission;
#[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
mod process;
mod providers;

fn capture(
    store: &SQLiteRecordStore,
    control: &StorageReadControl,
) -> Arc<dyn CommittedBackupSnapshot> {
    store.capture_backup_snapshot(control).unwrap().unwrap()
}

fn observe(store: &SQLiteRecordStore, namespace: &[u8], value: u64, control: &StorageReadControl) {
    store
        .allocate_identifiers(namespace, IdentifierRequest::Observe(value), control)
        .unwrap();
}

fn history_len(store: &SQLiteRecordStore) -> i64 {
    store
        .with(|connection| {
            Ok(connection.query_row(
                "SELECT count(*) FROM _uqa_mvcc_identifier_history",
                [],
                |row| row.get(0),
            )?)
        })
        .unwrap()
}

fn watermarks(
    snapshot: &dyn CommittedBackupSnapshot,
    control: &StorageReadControl,
) -> Vec<(Vec<u8>, u64)> {
    let mut found = Vec::new();
    snapshot
        .visit_identifier_watermarks(None, usize::MAX, control, &mut |key, value| {
            if let Some(value) = value {
                found.push((key.to_vec(), value));
            }
            Ok(true)
        })
        .unwrap();
    found
}

fn write(
    store: &SQLiteRecordStore,
    value: &[u8],
    expected: Option<CommitSequence>,
    control: &StorageReadControl,
) -> CommitSequence {
    let prepared = PreparedRecordCommit::new(
        &[RecordWrite {
            key: b"row",
            value: Some(value),
            expected,
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

#[test]
fn joint_cuts_keep_records_and_watermarks_while_writers_progress_in_every_file_mode() {
    for mode in 0..4 {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("joint.db");
        let connection = super::reclamation::open(&path, mode);
        let store = SQLiteRecordStore::new(&connection).unwrap();
        let control = control();
        observe(&store, b"a", 10, &control);
        let first_sequence = write(&store, b"first", None, &control);
        let first = capture(&store, &control);
        assert_eq!(first.metadata_epoch(), MetadataEpoch::INITIAL);
        assert_eq!(first.records().sequence(), first_sequence);
        assert_eq!(first.database_id(), store.identity);
        observe(&store, b"a", 20, &control);
        let second_sequence = write(&store, b"second", Some(first_sequence), &control);
        let second = capture(&store, &control);
        observe(&store, b"a", 30, &control);
        observe(&store, b"new", 50, &control);
        assert_eq!(second.records().sequence(), second_sequence);
        assert_eq!(second.metadata_epoch(), MetadataEpoch::from_u64(1));
        assert_eq!(store.reclaim_versions(&control).unwrap(), 0);
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
        assert_eq!(store.reclaim_versions(&control).unwrap(), 1);
        assert_eq!(history_len(&store), 1);
        drop(second);
        store.reclaim_versions(&control).unwrap();
        assert_eq!(history_len(&store), 0);
        drop(store);
        drop(connection);
        let reopened = SQLiteRecordStore::new(&super::reclamation::open(&path, mode)).unwrap();
        let latest = capture(&reopened, &control);
        assert_eq!(latest.records().sequence(), second_sequence);
        assert_eq!(
            watermarks(latest.as_ref(), &control),
            vec![(b"a".to_vec(), 30), (b"new".to_vec(), 50)]
        );
    }
}

#[test]
fn metadata_history_coalesces_updates_and_bounds_physical_positions() {
    let store = SQLiteRecordStore::new(&ManagedConnection::open_in_memory().unwrap()).unwrap();
    let control = control();
    observe(&store, b"a", 1, &control);
    let snapshot = capture(&store, &control);
    for value in 2..102 {
        observe(&store, b"a", value, &control);
    }
    for key in 0..8_u8 {
        observe(&store, &[b'z', key], 1, &control);
    }
    assert_eq!(history_len(&store), 1);
    let mut after = None;
    let mut positions = 0;
    let mut output = Vec::new();
    loop {
        let mut next = None;
        let mut count = 0;
        snapshot
            .visit_identifier_watermarks(after.as_deref(), 2, &control, &mut |key, value| {
                count += 1;
                next = Some(key.to_vec());
                if let Some(value) = value {
                    output.push((key.to_vec(), value));
                }
                Ok(true)
            })
            .unwrap();
        assert!(count <= 2);
        positions += count;
        if next.is_none() {
            break;
        }
        after = next;
        observe(&store, b"a", 102 + positions, &control);
    }
    assert_eq!(positions, 9);
    assert_eq!(output, vec![(b"a".to_vec(), 1)]);
    assert_eq!(
        store.snapshot(&control).unwrap().sequence(),
        CommitSequence::INITIAL
    );
}

#[test]
fn failed_allocation_rolls_back_the_watermark_and_its_archived_generation() {
    let store = SQLiteRecordStore::new(&ManagedConnection::open_in_memory().unwrap()).unwrap();
    let control = control();
    observe(&store, b"a", 7, &control);
    let snapshot = capture(&store, &control);
    store.with(|connection| {
        connection.execute_batch("CREATE TRIGGER reject_backup_identifier AFTER UPDATE ON _uqa_mvcc_identifiers BEGIN SELECT RAISE(ABORT, 'injected identifier publication failure'); END")?;
        Ok(())
    }).unwrap();
    assert!(store
        .allocate_identifiers(b"a", IdentifierRequest::Observe(8), &control)
        .is_err());
    assert_eq!(history_len(&store), 0);
    assert_eq!(
        watermarks(snapshot.as_ref(), &control),
        vec![(b"a".to_vec(), 7)]
    );
    store
        .with(|connection| {
            connection.execute_batch("DROP TRIGGER reject_backup_identifier")?;
            Ok(())
        })
        .unwrap();
    observe(&store, b"a", 9, &control);
    assert_eq!(history_len(&store), 1);
    assert_eq!(
        watermarks(snapshot.as_ref(), &control),
        vec![(b"a".to_vec(), 7)]
    );
}
