//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Compaction preserves exact logical bytes, ordered access and conditional-write evidence.

use std::collections::BTreeMap;
use uqa_storage::mvcc::{CommitReceipt, CommittedRecordSnapshot};

use super::*;

type Records = BTreeMap<Vec<u8>, Option<Vec<u8>>>;

#[test]
fn bounded_value_reads_reject_oversize_compacted_templates_before_loading() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let store = SQLiteRecordStore::new(&connection).unwrap();
    let records = (0..130)
        .map(|index| (key(b"bounded", index), Some(vec![b'x'; 64])))
        .collect::<Records>();
    publish(&store, &records, None);
    store.reclaim_versions(&control()).unwrap();
    assert!(count(&store, "_uqa_mvcc_runs") > 0);
    let snapshot = store.snapshot(&control()).unwrap();
    let query = StorageReadControl::with_limit(4096);
    let mut visited = false;
    let error = snapshot
        .visit_value_bounded(&key(b"bounded", 1), 63, &query, &mut |_| {
            visited = true;
            Ok(())
        })
        .unwrap_err()
        .into_storage_error();
    assert!(matches!(error, uqa_storage::StorageBackendError::Memory(_)));
    assert!(!visited);
    assert!(query.memory().peak() < 64);
    snapshot
        .visit_value_bounded(&key(b"bounded", 1), 64, &query, &mut |record| {
            assert_eq!(record.unwrap().value.unwrap(), [b'x'; 64]);
            Ok(())
        })
        .unwrap();
    assert_eq!(query.memory().used(), 0);
}

#[test]
fn acl_format_upgrade_retains_populated_runs_and_rejects_missing_predecessor_guards() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let store = SQLiteRecordStore::new(&connection).unwrap();
    let records = (0..130)
        .map(|index| (key(b"acl-format", index), Some(vec![b'x'; 64])))
        .collect::<Records>();
    let receipt = publish(&store, &records, None);
    store.reclaim_versions(&control()).unwrap();
    assert!(count(&store, "_uqa_mvcc_runs") > 0);
    let count_before = count(&store, "_uqa_mvcc_runs");
    super::downgrade_record_format(&store, 29);
    let upgraded = SQLiteRecordStore::new(&connection).unwrap();
    assert_eq!(count(&upgraded, "_uqa_mvcc_runs"), count_before);
    verify(
        upgraded.snapshot(&control()).unwrap().as_ref(),
        &records,
        receipt.sequence,
    );
    for sql in [
        "DROP TABLE _uqa_mvcc_runs",
        "DROP TRIGGER _uqa_mvcc_runs_UPDATE_guard",
    ] {
        let connection = ManagedConnection::open_in_memory().unwrap();
        let store = SQLiteRecordStore::new(&connection).unwrap();
        super::downgrade_record_format(&store, 29);
        store
            .with(|connection| {
                connection.execute_batch(sql)?;
                Ok(())
            })
            .unwrap();
        assert!(SQLiteRecordStore::new(&connection).is_err());
        store
            .with(|connection| {
                assert_eq!(
                    connection.query_row("SELECT format FROM _uqa_mvcc_metadata", [], |row| row
                        .get::<_, i64>(0))?,
                    29
                );
                Ok(())
            })
            .unwrap();
    }
}

fn key(prefix: &[u8], number: u64) -> Vec<u8> {
    [prefix, &number.to_be_bytes()].concat()
}

fn publish(
    store: &SQLiteRecordStore,
    records: &Records,
    expected: Option<CommitSequence>,
) -> CommitReceipt {
    let control = control();
    let writes: Vec<_> = records
        .iter()
        .map(|(key, value)| RecordWrite {
            key,
            expected,
            value: value.as_deref(),
        })
        .collect();
    let batch = PreparedRecordCommit::new(&writes, &control).unwrap();
    let id = store.allocate_transaction(&control).unwrap();
    let receipt = store.commit(id, &batch, &control).unwrap();
    assert_eq!(store.commit(id, &batch, &control).unwrap(), receipt);
    receipt
}

fn verify(snapshot: &dyn CommittedRecordSnapshot, records: &Records, sequence: CommitSequence) {
    let control = control();
    let mut actual = Records::new();
    let mut after = None;
    loop {
        let page = snapshot.scan(b"", after.as_deref(), 31, &control).unwrap();
        if page.is_empty() {
            break;
        }
        for row in page.iter() {
            assert_eq!(row.version.sequence(), sequence);
            actual.insert(
                row.key.to_vec(),
                row.version.value().map(|value| value.to_vec()),
            );
            after = Some(row.key.to_vec());
        }
    }
    assert_eq!(&actual, records);
    let mut keys = Vec::new();
    snapshot
        .visit_keys(b"", None, usize::MAX, &control, &mut |key, metadata| {
            assert_eq!(metadata.revision, Some(sequence));
            assert_eq!(metadata.live, records[key].is_some());
            keys.push(key.to_vec());
            Ok(true)
        })
        .unwrap();
    assert_eq!(keys, records.keys().cloned().collect::<Vec<_>>());
    for (key, value) in records {
        let row = snapshot.get(key, &control).unwrap().unwrap();
        assert_eq!(row.sequence(), sequence);
        assert_eq!(row.value().map(|value| value.to_vec()), *value);
        let metadata = snapshot.metadata(key, &control).unwrap().unwrap();
        assert_eq!(metadata.revision, Some(sequence));
        assert_eq!(metadata.live, value.is_some());
    }
}

fn count(store: &SQLiteRecordStore, table: &str) -> i64 {
    store
        .with(|connection| {
            Ok(
                connection.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                    row.get(0)
                })?,
            )
        })
        .unwrap()
}

#[test]
fn bounded_runs_preserve_values_tombstones_and_receipts_across_file_modes_and_reopen() {
    for mode in 0..4 {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("runs.db");
        let open = || {
            match mode {
                0 => ManagedConnection::open(&path),
                1 => ManagedConnection::open_encrypted(&path, "record-runs"),
                2 => ManagedConnection::open_compressed(
                    &path,
                    crate::SQLiteCompressionOptions::default(),
                ),
                _ => ManagedConnection::open_compressed_encrypted(
                    &path,
                    "record-runs",
                    crate::SQLiteCompressionOptions::default(),
                ),
            }
            .unwrap()
        };
        let mut records = Records::new();
        for number in 0..260 {
            records.insert(key(b"null", number), None);
            records.insert(key(b"empty", number), Some(Vec::new()));
            records.insert(key(b"constant", number), Some(b"constant".to_vec()));
            records.insert(
                key(b"suffix", number),
                Some(key(b"payload", number ^ (1 << 63))),
            );
        }
        let receipt;
        {
            let connection = open();
            let store = SQLiteRecordStore::new(&connection).unwrap();
            let empty = store.snapshot(&control()).unwrap();
            receipt = publish(&store, &records, None);
            assert_eq!(store.reclaim_versions(&control()).unwrap(), 0);
            assert_eq!(count(&store, "_uqa_mvcc_runs"), 0);
            assert!(empty.scan(b"", None, 1, &control()).unwrap().is_empty());
            drop(empty);
            let retained = store.snapshot(&control()).unwrap();
            assert_eq!(store.reclaim_versions(&control()).unwrap(), 0);
            assert_eq!(count(&store, "_uqa_mvcc_runs"), 12);
            assert_eq!(count(&store, "_uqa_mvcc_heads"), 0);
            assert_eq!(count(&store, "_uqa_mvcc_versions"), 0);
            verify(retained.as_ref(), &records, receipt.sequence);
            assert_eq!(store.reclaim_versions(&control()).unwrap(), 0);
            assert_eq!(
                store
                    .commit_status(receipt.transaction, &control())
                    .unwrap(),
                CommitStatus::Committed(receipt)
            );
        }
        let connection = open();
        let store = SQLiteRecordStore::new(&connection).unwrap();
        verify(
            store.snapshot(&control()).unwrap().as_ref(),
            &records,
            receipt.sequence,
        );
        assert_eq!(
            store
                .commit_status(receipt.transaction, &control())
                .unwrap(),
            CommitStatus::Committed(receipt)
        );
    }
}

#[test]
fn splitting_first_middle_and_last_members_preserves_retained_values_and_conflicts() {
    for kind in 0..4 {
        let connection = ManagedConnection::open_in_memory().unwrap();
        let store = SQLiteRecordStore::new(&connection).unwrap();
        let records: Records = (0..10)
            .map(|number| {
                (
                    key(b"row", number),
                    match kind {
                        0 => None,
                        1 => Some(Vec::new()),
                        2 => Some(b"constant".to_vec()),
                        _ => Some(key(b"value", number ^ 31)),
                    },
                )
            })
            .collect();
        let receipt = publish(&store, &records, None);
        store.reclaim_versions(&control()).unwrap();
        let retained = store.snapshot(&control()).unwrap();
        let changes: Records = [0, 4, 9].map(|number| (key(b"row", number), None)).into();
        let next = publish(&store, &changes, Some(receipt.sequence));
        assert_eq!(store.reclaim_versions(&control()).unwrap(), 0);
        verify(retained.as_ref(), &records, receipt.sequence);
        let latest = store.snapshot(&control()).unwrap();
        for (key, old) in &records {
            let row = latest.get(key, &control()).unwrap().unwrap();
            if changes.contains_key(key) {
                assert_eq!(row.sequence(), next.sequence);
                assert!(row.value().is_none());
            } else {
                assert_eq!(row.sequence(), receipt.sequence);
                assert_eq!(row.value().map(|value| value.to_vec()), *old);
            }
            let stale = prepared(key, b"must conflict", &control());
            let id = store.allocate_transaction(&control()).unwrap();
            assert!(matches!(
                store.commit(id, &stale, &control()),
                Err(CommitFailure::Rejected(VersionError::WriteConflict { .. }))
            ));
        }
        drop(retained);
        assert_eq!(store.reclaim_versions(&control()).unwrap(), 3);
        for key in changes.keys() {
            assert!(latest
                .get(key, &control())
                .unwrap()
                .unwrap()
                .value()
                .is_none());
        }
    }
}

#[test]
fn ordered_scans_merge_interleaved_key_lengths_prefixes_gaps_and_maximum_suffixes() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let store = SQLiteRecordStore::new(&connection).unwrap();
    let mut records: Records = (0..6).map(|number| (key(b"", number), None)).collect();
    let receipt = publish(&store, &records, None);
    store.reclaim_versions(&control()).unwrap();
    let mut additional: Records = (0..6)
        .map(|number| (key(&key(b"", 2), number), Some(vec![7])))
        .collect();
    for number in [0, 2, 4, u64::MAX - 1, u64::MAX] {
        additional.insert(key(b"\xff", number), Some(vec![4]));
    }
    additional.insert(b"short".to_vec(), Some(vec![1; 129]));
    publish(&store, &additional, None);
    store.reclaim_versions(&control()).unwrap();
    records.extend(additional);
    let snapshot = store.snapshot(&control()).unwrap();
    let mut actual = Records::new();
    snapshot
        .visit_prefix(b"", None, usize::MAX, &control(), &mut |key, row| {
            actual.insert(key.to_vec(), row.value.map(<[u8]>::to_vec));
            Ok(true)
        })
        .unwrap();
    assert_eq!(actual, records);
    for prefix in [vec![], vec![0], key(b"", 2), vec![255], b"short".to_vec()] {
        for after in [
            None,
            Some(vec![0; 9]),
            Some(key(b"", 2)),
            Some(key(b"\xff", 3)),
            Some(vec![255; 12]),
        ] {
            for limit in [0, 1, 3, 100] {
                let expected: Vec<_> = records
                    .iter()
                    .filter(|(key, _)| {
                        key.starts_with(&prefix)
                            && after.as_deref().is_none_or(|after| &key[..] > after)
                    })
                    .take(limit)
                    .map(|(key, _)| key.clone())
                    .collect();
                let page = snapshot
                    .scan(&prefix, after.as_deref(), limit, &control())
                    .unwrap();
                assert_eq!(
                    page.iter().map(|row| row.key.to_vec()).collect::<Vec<_>>(),
                    expected
                );
            }
        }
    }
    // The first compressed range retains its original revision through interleaved publication.
    assert_eq!(
        snapshot
            .get(&key(b"", 2), &control())
            .unwrap()
            .unwrap()
            .sequence(),
        receipt.sequence
    );
    assert!(snapshot
        .get(&key(b"\xff", 3), &control())
        .unwrap()
        .is_none());
}

#[test]
fn failed_compaction_and_failed_range_extraction_restore_the_complete_original_state() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let store = SQLiteRecordStore::new(&connection).unwrap();
    let records: Records = (0..8)
        .map(|number| (key(b"row", number), Some(vec![9])))
        .collect();
    let receipt = publish(&store, &records, None);
    store.with(|connection| { connection.execute_batch("CREATE TRIGGER reject_runs BEFORE DELETE ON _uqa_mvcc_heads BEGIN SELECT RAISE(ABORT, 'injected compaction failure'); END")?; Ok(()) }).unwrap();
    assert!(store.reclaim_versions(&control()).is_err());
    assert_eq!(count(&store, "_uqa_mvcc_runs"), 0);
    verify(
        store.snapshot(&control()).unwrap().as_ref(),
        &records,
        receipt.sequence,
    );
    store
        .with(|connection| {
            connection.execute_batch("DROP TRIGGER reject_runs")?;
            Ok(())
        })
        .unwrap();
    store.reclaim_versions(&control()).unwrap();
    store.with(|connection| { connection.execute_batch("CREATE TRIGGER reject_split BEFORE INSERT ON _uqa_mvcc_runs BEGIN SELECT RAISE(ABORT, 'injected split failure'); END")?; Ok(()) }).unwrap();
    let changed_key = key(b"row", 3);
    let batch = PreparedRecordCommit::new(
        &[RecordWrite {
            key: &changed_key,
            expected: Some(receipt.sequence),
            value: None,
        }],
        &control(),
    )
    .unwrap();
    let id = store.allocate_transaction(&control()).unwrap();
    assert!(matches!(
        store.commit(id, &batch, &control()),
        Err(CommitFailure::Rejected(_))
    ));
    assert_eq!(
        store.commit_status(id, &control()).unwrap(),
        CommitStatus::Pending
    );
    assert_eq!(count(&store, "_uqa_mvcc_runs"), 1);
    assert_eq!(count(&store, "_uqa_mvcc_versions"), 0);
    verify(
        store.snapshot(&control()).unwrap().as_ref(),
        &records,
        receipt.sequence,
    );
    store
        .with(|connection| {
            connection.execute_batch("DROP TRIGGER reject_split")?;
            Ok(())
        })
        .unwrap();
    let next = store.commit(id, &batch, &control()).unwrap();
    assert_eq!(store.commit(id, &batch, &control()).unwrap(), next);
}

#[test]
fn run_metadata_avoids_payload_allocation_and_value_reads_obey_memory_and_cancellation() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let store = SQLiteRecordStore::new(&connection).unwrap();
    let records: Records = (0..4)
        .map(|number| (key(b"row", number), Some(vec![42; 128])))
        .collect();
    publish(&store, &records, None);
    store.reclaim_versions(&control()).unwrap();
    let snapshot = store.snapshot(&control()).unwrap();
    let key = key(b"row", 1);
    let limited = StorageReadControl::with_limit(96);
    assert!(snapshot.metadata(&key, &limited).unwrap().unwrap().live);
    assert!(matches!(
        snapshot.get(&key, &limited),
        Err(VersionError::Memory(_))
    ));
    let cancelled = control();
    assert!(matches!(
        snapshot.visit_value(&key, &cancelled, &mut |record| {
            assert_eq!(record.unwrap().value.unwrap(), &[42; 128]);
            cancelled.cancellation().cancel();
            Ok(())
        }),
        Err(VersionError::Cancelled(_))
    ));
    assert_eq!(limited.memory().used(), 0);
}

#[test]
fn corrupt_run_extents_lengths_and_template_types_are_rejected() {
    for mutation in [
        "UPDATE _uqa_mvcc_runs SET last_key = x'726f770000000000000080'",
        "UPDATE _uqa_mvcc_runs SET key_length = 12",
        "UPDATE _uqa_mvcc_runs SET value = 'invalid blob'",
        "UPDATE _uqa_mvcc_runs SET last_key = x'726f780000000000000003'",
    ] {
        let connection = ManagedConnection::open_in_memory().unwrap();
        let store = SQLiteRecordStore::new(&connection).unwrap();
        let records: Records = (0..4)
            .map(|number| (key(b"row", number), Some(vec![42; 16])))
            .collect();
        publish(&store, &records, None);
        store.reclaim_versions(&control()).unwrap();
        store
            .with(|connection| {
                let _permit = schema::WritePermit::acquire(connection)?;
                connection.execute_batch("PRAGMA ignore_check_constraints = ON")?;
                connection.execute_batch(mutation)?;
                connection.execute_batch("PRAGMA ignore_check_constraints = OFF")?;
                Ok(())
            })
            .unwrap();
        let snapshot = store.snapshot(&control()).unwrap();
        assert!(matches!(
            snapshot.scan(b"", None, 1, &control()),
            Err(VersionError::InvalidEncoding(_))
        ));
    }
}
