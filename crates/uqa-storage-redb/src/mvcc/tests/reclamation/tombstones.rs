//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_storage::mvcc::{RecordWrite, TombstoneReclamationRequest, TombstoneReclamationStep};

#[test]
fn tombstone_reclamation_preserves_absence_guards_receipts_and_epochs_across_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("retired-records.redb");
    let control = StorageReadControl::with_limit(1 << 20);
    {
        let store = RedbRecordStore::new(Arc::new(Database::create(&path).unwrap())).unwrap();
        uqa_storage::mvcc::verify_tombstone_reclamation(&store).unwrap();
        uqa_storage::mvcc::verify_tombstone_reclamation_pages(&store).unwrap();
        let read = store.database.begin_read().unwrap();
        assert_eq!(
            read.open_table(HEADS)
                .unwrap()
                .range(b"pages/".as_slice()..b"pages0".as_slice())
                .unwrap()
                .count(),
            1
        );
        assert_eq!(
            read.open_table(VERSIONS)
                .unwrap()
                .range((b"pages/".as_slice(), 0)..(b"pages0".as_slice(), 0))
                .unwrap()
                .count(),
            1
        );
    }
    let store = RedbRecordStore::new(Arc::new(Database::create(&path).unwrap())).unwrap();
    let snapshot = store.snapshot(&control).unwrap();
    assert_eq!(snapshot.reclamation_epoch(), Some(4));
    assert!(snapshot
        .get(b"pages/000/unique-0", &control)
        .unwrap()
        .is_none());
    let prepared = PreparedRecordCommit::new(
        &[RecordWrite {
            key: b"retired/key",
            expected: None,
            value: Some(b"stale"),
        }],
        &control,
    )
    .unwrap();
    let id = store.allocate_transaction(&control).unwrap();
    assert!(matches!(
        store.commit(id, &prepared, &control),
        Err(CommitFailure::Rejected(
            VersionError::ReclaimedObservation { .. }
        ))
    ));
}

#[test]
fn tombstone_reclamation_failure_recovers_atomic_epochs_and_preserves_original_receipts() {
    let backend = FaultBackend::default();
    let control = StorageReadControl::with_limit(1 << 20);
    let prepared = PreparedRecordCommit::new(
        &[RecordWrite {
            key: b"retired/key",
            expected: None,
            value: None,
        }],
        &control,
    )
    .unwrap();
    let (id, receipt) = {
        let database = Arc::new(
            Database::builder()
                .create_with_backend(backend.clone())
                .unwrap(),
        );
        let store = RedbRecordStore::new(database.clone()).unwrap();
        let peer = RedbRecordStore::new(database).unwrap();
        let id = store.allocate_transaction(&control).unwrap();
        let receipt = store.commit(id, &prepared, &control).unwrap();
        store.reclaim_versions(&control).unwrap();
        let request = TombstoneReclamationRequest {
            prefix: b"retired/",
            after: None,
            through: receipt.sequence,
        };
        let retained = peer.snapshot(&control).unwrap();
        assert!(matches!(
            store.reclaim_tombstones(&request, &control).unwrap(),
            TombstoneReclamationStep::Retained
        ));
        drop(retained);
        backend.fail_sync.store(true, Ordering::Relaxed);
        assert!(store.reclaim_tombstones(&request, &control).is_err());
        assert!(!backend.fail_sync.load(Ordering::Relaxed));
        (id, receipt)
    };
    let database = Arc::new(Database::builder().create_with_backend(backend).unwrap());
    let store = RedbRecordStore::new(database).unwrap();
    let snapshot = store.snapshot(&control).unwrap();
    match snapshot.reclamation_epoch().unwrap() {
        0 => assert_eq!(
            snapshot
                .get(b"retired/key", &control)
                .unwrap()
                .unwrap()
                .sequence(),
            receipt.sequence
        ),
        1 => assert!(snapshot.get(b"retired/key", &control).unwrap().is_none()),
        epoch => panic!("incomplete retirement epoch {epoch}"),
    }
    assert_eq!(snapshot.sequence(), receipt.sequence);
    assert_eq!(store.commit(id, &prepared, &control).unwrap(), receipt);
    drop(snapshot);
    let request = TombstoneReclamationRequest {
        prefix: b"retired/",
        after: None,
        through: receipt.sequence,
    };
    store.reclaim_tombstones(&request, &control).unwrap();
    assert_eq!(
        store.snapshot(&control).unwrap().reclamation_epoch(),
        Some(1)
    );
    let read = store.database.begin_read().unwrap();
    assert_eq!(read.open_table(HEADS).unwrap().len().unwrap(), 0);
    assert_eq!(read.open_table(VERSIONS).unwrap().len().unwrap(), 0);
}
