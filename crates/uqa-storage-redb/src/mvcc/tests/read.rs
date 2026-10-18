//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Head lookups and historical ranges share snapshot and tombstone semantics.

use super::*;
use uqa_storage::mvcc::RecordWrite;

#[test]
fn bounded_value_reads_use_the_shared_redb_owner_and_retained_versions() {
    let directory = tempfile::tempdir().unwrap();
    let owner = crate::RedbStorage::open(directory.path().join("bounded.redb")).unwrap();
    uqa_storage::key_value::conformance::verify_bounded_value_reads(&owner.store()).unwrap();
}

fn assert_point_reads(snapshot: &dyn CommittedRecordSnapshot, control: &StorageReadControl) {
    let requested = [b"d".as_slice(), b"b", b"a", b"b", b"c"];
    let mut keys = requested.into_iter().map(|key| {
        let mut bytes = uqa_core::memory::BudgetedVec::new(control.memory());
        bytes.extend_from_slice(key)?;
        Ok(bytes)
    });
    let before = crate::mvcc::read::READ_WINDOWS.get();
    let mut points = Vec::new();
    snapshot
        .visit_values(&mut keys, control, &mut |key, row| {
            points.push((
                key.to_vec(),
                row.map(|row| (row.revision, row.value.map(<[u8]>::to_vec))),
            ));
            Ok(true)
        })
        .unwrap();
    assert_eq!(crate::mvcc::read::READ_WINDOWS.get() - before, 1);
    for (key, row) in points {
        let wanted = snapshot.get(&key, control).unwrap();
        assert_eq!(
            row,
            wanted.map(|row| (
                Some(row.sequence()),
                row.value().map(|value| value.to_vec())
            ))
        );
    }
}

#[test]
fn head_and_history_reads_preserve_missing_values_tombstones_and_order() {
    let database = Arc::new(
        Database::builder()
            .create_with_backend(InMemoryBackend::new())
            .unwrap(),
    );
    let store = RedbRecordStore::new(database).unwrap();
    let control = StorageReadControl::with_limit(1 << 20);
    let empty = store.snapshot(&control).unwrap();
    let first = store.allocate_transaction(&control).unwrap();
    let initial = PreparedRecordCommit::new(
        &[
            RecordWrite {
                key: b"a",
                expected: None,
                value: Some(b"old"),
            },
            RecordWrite {
                key: b"b",
                expected: None,
                value: Some(b"kept"),
            },
        ],
        &control,
    )
    .unwrap();
    let first = store.commit(first, &initial, &control).unwrap();
    let older = store.snapshot(&control).unwrap();
    let replacement = PreparedRecordCommit::new(
        &[
            RecordWrite {
                key: b"a",
                expected: Some(first.sequence),
                value: None,
            },
            RecordWrite {
                key: b"c",
                expected: None,
                value: Some(b"new"),
            },
        ],
        &control,
    )
    .unwrap();
    let second = store.allocate_transaction(&control).unwrap();
    let second = store.commit(second, &replacement, &control).unwrap();
    let latest = store.snapshot(&control).unwrap();
    assert_eq!(empty.sequence(), CommitSequence::INITIAL);
    assert_eq!(older.sequence(), first.sequence);
    assert_eq!(latest.sequence(), second.sequence);
    for (snapshot, expected) in [
        (empty, vec![]),
        (
            older,
            vec![
                (b"a".to_vec(), Some(b"old".to_vec())),
                (b"b".to_vec(), Some(b"kept".to_vec())),
            ],
        ),
        (
            latest,
            vec![
                (b"a".to_vec(), None),
                (b"b".to_vec(), Some(b"kept".to_vec())),
                (b"c".to_vec(), Some(b"new".to_vec())),
            ],
        ),
    ] {
        for key in [b"a", b"b", b"c", b"d"] {
            let row = snapshot.get(key, &control).unwrap();
            let wanted = expected.iter().find(|(name, _)| name == key);
            assert_eq!(row.is_some(), wanted.is_some());
            assert_eq!(
                row.as_ref()
                    .and_then(|row| row.value().map(|value| value.to_vec())),
                wanted.and_then(|(_, value)| value.clone())
            );
            assert_eq!(
                snapshot
                    .metadata(key, &control)
                    .unwrap()
                    .map(|record| record.live),
                wanted.map(|(_, value)| value.is_some())
            );
        }
        assert_point_reads(snapshot.as_ref(), &control);
        let mut visited = Vec::new();
        snapshot
            .visit_prefix(b"", None, 10, &control, &mut |key, row| {
                visited.push((key.to_vec(), row.value.map(<[u8]>::to_vec)));
                Ok(true)
            })
            .unwrap();
        assert_eq!(visited, expected);
        let page = snapshot.scan(b"", Some(b"a"), 1, &control).unwrap();
        assert_eq!(page.len(), usize::from(!expected.is_empty()));
        if let Some(row) = page.first() {
            assert_eq!(&*row.key, b"b");
        }
    }
}

#[test]
fn a_visible_head_cannot_silently_point_to_a_missing_version() {
    let database = Arc::new(
        Database::builder()
            .create_with_backend(InMemoryBackend::new())
            .unwrap(),
    );
    let store = RedbRecordStore::new(database.clone()).unwrap();
    let control = StorageReadControl::with_limit(1 << 20);
    let write = PreparedRecordCommit::new(
        &[RecordWrite {
            key: b"a",
            expected: None,
            value: Some(b"value"),
        }],
        &control,
    )
    .unwrap();
    let id = store.allocate_transaction(&control).unwrap();
    let receipt = store.commit(id, &write, &control).unwrap();
    let transaction = physical_writer(&database).unwrap();
    transaction
        .open_table(VERSIONS)
        .unwrap()
        .remove((b"a".as_slice(), receipt.sequence.as_u64()))
        .unwrap();
    transaction.commit().unwrap();
    let snapshot = store.snapshot(&control).unwrap();
    assert!(matches!(
        snapshot.get(b"a", &control),
        Err(VersionError::InvalidEncoding("record head has no version"))
    ));
    assert!(matches!(
        snapshot.scan(b"", None, 1, &control),
        Err(VersionError::InvalidEncoding("record head has no version"))
    ));
}

#[test]
fn point_read_windows_are_lazy_borrowed_and_stop_before_another_key() {
    let directory = tempfile::tempdir().unwrap();
    let owner = crate::RedbStorage::open(directory.path().join("points.redb")).unwrap();
    let store = owner.record_store().unwrap();
    let setup = StorageReadControl::with_limit(1 << 20);
    let payload = vec![71; 100_000];
    let prepared = PreparedRecordCommit::new(
        &[RecordWrite {
            key: b"large",
            expected: None,
            value: Some(&payload),
        }],
        &setup,
    )
    .unwrap();
    let id = store.allocate_transaction(&setup).unwrap();
    store.commit(id, &prepared, &setup).unwrap();
    let snapshot = store.snapshot(&setup).unwrap();
    for scenario in ["empty", "stop", "cancel", "visitor", "producer"] {
        let control = StorageReadControl::with_limit(64);
        let produced = std::cell::Cell::new(0);
        let mut visited = 0;
        let before = crate::mvcc::read::READ_WINDOWS.get();
        let mut keys = std::iter::from_fn(|| {
            if scenario == "empty" || produced.get() == 3 {
                return None;
            }
            produced.set(produced.get() + 1);
            if scenario == "producer" && produced.get() == 2 {
                return Some(Err(VersionError::InvalidEncoding(
                    "injected producer failure",
                )));
            }
            let mut key = uqa_core::memory::BudgetedVec::new(control.memory());
            Some(
                key.extend_from_slice(b"large")
                    .map(|()| key)
                    .map_err(Into::into),
            )
        });
        let result = snapshot.visit_values(&mut keys, &control, &mut |key, record| {
            visited += 1;
            assert_eq!(key, b"large");
            assert_eq!(record.unwrap().value.unwrap(), payload);
            match scenario {
                "stop" => Ok(false),
                "cancel" => {
                    control.cancellation().cancel();
                    Ok(true)
                }
                "visitor" => Err(VersionError::InvalidEncoding("injected visitor failure")),
                _ => Ok(true),
            }
        });
        assert_eq!(
            result.is_ok(),
            matches!(scenario, "empty" | "stop"),
            "{scenario}"
        );
        assert_eq!(visited, usize::from(scenario != "empty"));
        assert_eq!(
            produced.get(),
            match scenario {
                "empty" => 0,
                "producer" => 2,
                _ => 1,
            }
        );
        assert_eq!(
            crate::mvcc::read::READ_WINDOWS.get() - before,
            usize::from(scenario != "empty")
        );
        assert_eq!(control.memory().used(), 0);
    }
}
