//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn reclamation_preserves_live_views_tombstones_and_receipts_across_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("reclamation.redb");
    let control = StorageReadControl::with_limit(1 << 20);
    {
        let store = RedbRecordStore::new(Arc::new(Database::create(&path).unwrap())).unwrap();
        uqa_storage::mvcc::verify_version_reclamation(&store).unwrap();
        let read = store.database.begin_read().unwrap();
        assert_eq!(read.open_table(VERSIONS).unwrap().len().unwrap(), 2);
        assert_eq!(read.open_table(HEADS).unwrap().len().unwrap(), 2);
        assert_eq!(read.open_table(TRANSACTIONS).unwrap().len().unwrap(), 4);
    }
    let store = RedbRecordStore::new(Arc::new(Database::create(&path).unwrap())).unwrap();
    let snapshot = store.snapshot(&control).unwrap();
    assert!(snapshot
        .get(b"retention-a", &control)
        .unwrap()
        .unwrap()
        .value()
        .is_none());
    assert_eq!(
        &***snapshot
            .get(b"retention-b", &control)
            .unwrap()
            .unwrap()
            .value()
            .unwrap(),
        b"later"
    );
    assert_eq!(store.reclaim_versions(&control).unwrap(), 0);
}

#[test]
fn record_adapters_over_the_same_physical_owner_share_retention() {
    let database = Arc::new(
        Database::builder()
            .create_with_backend(InMemoryBackend::new())
            .unwrap(),
    );
    let first = RedbRecordStore::new(Arc::clone(&database)).unwrap();
    let second = RedbRecordStore::new(database).unwrap();
    assert!(Arc::ptr_eq(&first.snapshots, &second.snapshots));
    let control = StorageReadControl::with_limit(1 << 20);
    let snapshot = first.snapshot(&control).unwrap();
    let weak = Arc::downgrade(&first.snapshots);
    drop(first);
    drop(second);
    assert!(weak.upgrade().is_some());
    drop(snapshot);
    assert!(weak.upgrade().is_none());
}
