//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Post-capture keys advance the physical cursor without becoming image records.

use super::*;

fn seed(store: &SQLiteRecordStore, setup: &StorageReadControl) -> CommitSequence {
    let transaction = store.allocate_transaction(setup).unwrap();
    store
        .commit(
            transaction,
            &PreparedRecordCommit::new(
                &[
                    RecordWrite {
                        key: b"",
                        expected: None,
                        value: Some(b"empty key"),
                    },
                    RecordWrite {
                        key: b"dead",
                        expected: None,
                        value: None,
                    },
                    RecordWrite {
                        key: b"z",
                        expected: None,
                        value: Some(b"last"),
                    },
                ],
                setup,
            )
            .unwrap(),
            setup,
        )
        .unwrap()
        .sequence
}

fn verify_positions(store: &SQLiteRecordStore) {
    let setup = StorageReadControl::with_limit(8 << 20);
    let empty = capture(store, &setup);
    assert!(empty.record_key_ceiling(&setup).unwrap().is_none());
    assert!(empty
        .identifier_namespace_ceiling(&setup)
        .unwrap()
        .is_none());
    drop(empty);
    let first = seed(store, &setup);
    observe(store, b"ids", 10, &setup);
    let control = StorageReadControl::with_limit(4096);
    let cut = capture(store, &control);
    let keys: Vec<_> = (0..180)
        .map(|i| format!("key-{i:03}").into_bytes())
        .collect();
    let writes: Vec<_> = keys
        .iter()
        .map(|key| RecordWrite {
            key,
            expected: None,
            value: Some(b"future"),
        })
        .collect();
    let transaction = store.allocate_transaction(&setup).unwrap();
    store
        .commit(
            transaction,
            &PreparedRecordCommit::new(&writes, &setup).unwrap(),
            &setup,
        )
        .unwrap();
    let ceiling = cut.record_key_ceiling(&control).unwrap().unwrap();
    assert_eq!(&*ceiling, b"z");
    observe(store, b"zz", 20, &setup);
    let identifier_ceiling = cut.identifier_namespace_ceiling(&control).unwrap().unwrap();
    assert_eq!(&*identifier_ceiling, b"zz");
    let transaction = store.allocate_transaction(&setup).unwrap();
    store
        .commit(
            transaction,
            &PreparedRecordCommit::new(
                &[
                    RecordWrite {
                        key: b"zz",
                        expected: None,
                        value: Some(b"beyond ceiling"),
                    },
                    RecordWrite {
                        key: b"z",
                        expected: Some(first),
                        value: None,
                    },
                ],
                &setup,
            )
            .unwrap(),
            &setup,
        )
        .unwrap();
    store.reclaim_versions(&setup).unwrap();
    assert_positions(cut.as_ref(), &ceiling, first, &control);
    let mut called = false;
    cut.visit_record_positions(None, 0, &control, &mut |_, _| {
        called = true;
        Ok(true)
    })
    .unwrap();
    assert!(!called);
    assert!(matches!(
        cut.visit_record_positions(None, 1, &control, &mut |_, _| Err(
            VersionError::InvalidEncoding("visitor failed")
        )),
        Err(VersionError::InvalidEncoding("visitor failed"))
    ));
    observe(store, b"ids", 30, &setup);
    assert_eq!(
        watermarks(cut.as_ref(), &control),
        vec![(b"ids".to_vec(), 10)]
    );
    let cancelled = StorageReadControl::with_limit(0);
    cancelled.cancellation().cancel();
    assert!(matches!(
        cut.record_key_ceiling(&cancelled),
        Err(VersionError::Cancelled(_))
    ));
    assert!(matches!(
        cut.identifier_namespace_ceiling(&cancelled),
        Err(VersionError::Cancelled(_))
    ));
    drop(identifier_ceiling);
    drop(ceiling);
    drop(cut);
    assert_eq!(control.memory().used(), 0);
}

fn assert_positions(
    cut: &dyn CommittedBackupSnapshot,
    ceiling: &[u8],
    first: CommitSequence,
    control: &StorageReadControl,
) {
    let mut after: Option<Vec<u8>> = None;
    let mut visible = Vec::new();
    let mut physical = 0;
    let mut finished = false;
    while !finished {
        let mut count = 0;
        let mut next = None;
        cut.visit_record_positions(after.as_deref(), 7, control, &mut |key, metadata| {
            count += 1;
            physical += 1;
            if key > ceiling {
                finished = true;
                return Ok(false);
            }
            if let Some(metadata) = metadata {
                assert_eq!(metadata.revision, Some(first));
                visible.push((key.to_vec(), metadata.live));
            }
            next = Some(key.to_vec());
            Ok(true)
        })
        .unwrap();
        assert!(count <= 7);
        if count < 7 {
            finished = true;
        }
        if let Some(next) = next {
            after = Some(next);
        }
    }
    assert_eq!(physical, 184);
    assert_eq!(
        visible,
        vec![
            (b"".to_vec(), true),
            (b"dead".to_vec(), false),
            (b"z".to_vec(), true)
        ]
    );
}

#[test]
fn backup_record_positions_bound_absent_keys_and_retain_the_cut() {
    verify_positions(
        &SQLiteRecordStore::new(&ManagedConnection::open_in_memory().unwrap()).unwrap(),
    );
}
