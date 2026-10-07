//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Batched allocations preserve durable outcomes, quotas and independent owners.

use super::*;

#[test]
fn managed_allocations_share_one_sync_without_relaxing_durable_receipts() {
    let directory = tempfile::tempdir().unwrap();
    let connection = ManagedConnection::open(&directory.path().join("batched.db")).unwrap();
    let store = SQLiteRecordStore::new(&connection).unwrap();
    let control = control();
    let commits = record_commits(&store);
    let mut owners = Vec::new();
    for expected in 1..=63 {
        if expected == 32 {
            // The first few requests grow the reserve from one receipt; an
            // owner used only once does not retain a batch of unused receipts.
            commits.lock().clear();
        }
        let owner = store.allocate_managed_transaction(&control).unwrap();
        assert_eq!(owner.transaction().allocation(), expected);
        assert_eq!(
            store.commit_status(owner.transaction(), &control).unwrap(),
            CommitStatus::Pending
        );
        owners.push(owner);
    }
    assert_eq!(*commits.lock(), [false, false]);
    // A separately opened provider sees every returned receipt, and neither
    // manual nor managed allocations can reuse the reserved identities.
    let reopened = SQLiteRecordStore::new(&connection).unwrap();
    for owner in &owners {
        assert_eq!(
            reopened
                .commit_status(owner.transaction(), &control)
                .unwrap(),
            CommitStatus::Pending
        );
    }
    assert_eq!(
        reopened
            .allocate_transaction(&control)
            .unwrap()
            .allocation(),
        64
    );
    assert_eq!(store.reclaim_transaction_receipts(&control).unwrap(), 0);
    drop(owners);
    assert_eq!(store.reclaim_transaction_receipts(&control).unwrap(), 63);
}

#[test]
fn cached_allocations_obey_the_new_callers_memory_and_cancellation() {
    let directory = tempfile::tempdir().unwrap();
    let connection = ManagedConnection::open(&directory.path().join("caller.db")).unwrap();
    let store = SQLiteRecordStore::new(&connection).unwrap();
    let control = control();
    let first = store.allocate_managed_transaction(&control).unwrap();
    assert!(store
        .allocate_managed_transaction(&StorageReadControl::with_limit(0))
        .is_err());
    let cancelled = StorageReadControl::with_limit(1 << 20);
    cancelled.cancellation().cancel();
    assert!(store.allocate_managed_transaction(&cancelled).is_err());
    let second = store.allocate_managed_transaction(&control).unwrap();
    assert_eq!(
        second.transaction().allocation(),
        first.transaction().allocation() + 1
    );
    let peer = SQLiteRecordStore::new(&connection).unwrap();
    assert_eq!(peer.reclaim_transaction_receipts(&control).unwrap(), 0);
    // Store clones own the reserve together; its unused Pending receipts become
    // reclaimable only after the last store disappears, without erasing owners.
    let clone = store.clone();
    drop(store);
    assert_eq!(peer.reclaim_transaction_receipts(&control).unwrap(), 0);
    drop(clone);
    assert_eq!(peer.reclaim_transaction_receipts(&control).unwrap(), 1);
    assert_eq!(
        peer.commit_status(first.transaction(), &control).unwrap(),
        CommitStatus::Pending
    );
    assert_eq!(
        peer.commit_status(second.transaction(), &control).unwrap(),
        CommitStatus::Pending
    );
    drop((first, second));
    assert_eq!(peer.reclaim_transaction_receipts(&control).unwrap(), 2);
}

#[test]
fn managed_batches_never_overdraw_the_shared_receipt_limit() {
    let directory = tempfile::tempdir().unwrap();
    let connection = ManagedConnection::open(&directory.path().join("quota.db")).unwrap();
    let store = SQLiteRecordStore::new(&connection).unwrap();
    let control = control();
    store.set_receipt_retention_limit(8, &control).unwrap();
    let mut owners = Vec::new();
    for _ in 0..8 {
        owners.push(store.allocate_managed_transaction(&control).unwrap());
    }
    assert!(matches!(
        store.allocate_managed_transaction(&control),
        Err(VersionError::ReceiptRetentionExhausted { limit: 8 })
    ));
    drop(owners);
    assert_eq!(store.reclaim_transaction_receipts(&control).unwrap(), 8);
    assert!(store.allocate_managed_transaction(&control).is_ok());
}
