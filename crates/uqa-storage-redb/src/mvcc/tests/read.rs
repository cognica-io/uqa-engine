//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Head lookups and historical ranges share snapshot and tombstone semantics.

use super::*;
use uqa_storage::mvcc::RecordWrite;

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
    store.commit(second, &replacement, &control).unwrap();
    let latest = store.snapshot(&control).unwrap();
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
