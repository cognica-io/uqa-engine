//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Rejected admission and uncertain physical completion preserve exact receipt recovery.

use super::*;

#[test]
fn cancelled_and_exhausted_receipt_operations_preserve_prior_owners_and_watermarks() {
    let store = memory();
    let control = StorageReadControl::with_limit(1 << 20);
    let zero = StorageReadControl::with_limit(0);
    assert!(matches!(
        store.allocate_managed_transaction(&zero),
        Err(VersionError::Memory(_))
    ));
    assert_eq!(count(&store), 0);
    let retained = store.allocate_managed_transaction(&control).unwrap();
    let id = retained.transaction();
    assert_eq!(id.allocation(), 1);
    store.abort(id, &control).unwrap();
    let cancelled = StorageReadControl::with_limit(1 << 20);
    cancelled.cancellation().cancel();
    assert!(matches!(
        store.acknowledge_transaction(ReceiptAcknowledgement::Aborted(id), &cancelled),
        Err(VersionError::Cancelled(_))
    ));
    assert!(matches!(
        store.reclaim_transaction_receipts(&cancelled),
        Err(VersionError::Cancelled(_))
    ));
    assert!(matches!(
        store.allocate_managed_transaction(&cancelled),
        Err(VersionError::Cancelled(_))
    ));
    assert!(matches!(
        store.set_receipt_retention_limit(1, &cancelled),
        Err(VersionError::Cancelled(_))
    ));
    assert_eq!(store.reclaim_transaction_receipts(&control).unwrap(), 0);
    store
        .acknowledge_transaction(ReceiptAcknowledgement::Aborted(id), &control)
        .unwrap();
    assert!(matches!(
        store.reclaim_transaction_receipts(&zero),
        Err(VersionError::Memory(_))
    ));
    assert_eq!(
        store.commit_status(id, &control).unwrap(),
        CommitStatus::Aborted
    );
    assert_eq!(store.reclaim_transaction_receipts(&control).unwrap(), 1);
    drop(retained);
    store.reclaim_transaction_receipts(&control).unwrap();
    assert_eq!(zero.memory().used(), 0);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn unknown_ssi_publications_block_receipt_collection_without_guessing_an_abort() {
    let store = memory();
    let control = StorageReadControl::with_limit(1 << 20);
    let retained = store.allocate_managed_transaction(&control).unwrap();
    let id = retained.transaction();
    let (participant, snapshot) = store.admit_serializable_snapshot(false, &control).unwrap();
    let unknown = StorageTransactionId::new(store.identity, id.allocation() + 1).unwrap();
    store
        .with_serializable_admission(&control, &mut |graph, _| {
            graph.prepare_publication(participant.id(), unknown, [9; 32], &control)?;
            Ok(())
        })
        .unwrap();
    drop((retained, participant, snapshot));
    assert!(matches!(
        store.reclaim_transaction_receipts(&control),
        Err(VersionError::UnknownTransaction)
    ));
    assert_eq!(
        store.commit_status(id, &control).unwrap(),
        CommitStatus::Pending
    );
    assert_eq!(count(&store), 1);
}

#[test]
fn malformed_receipts_and_limits_fail_closed_without_deleting_or_repairing_rows() {
    for bytes in [vec![], vec![5], vec![0x10], vec![3, 0], vec![4], vec![0x0a]] {
        let store = memory();
        let control = StorageReadControl::with_limit(1 << 20);
        let id = store.allocate_transaction(&control).unwrap();
        let transaction = physical_writer(&store.database).unwrap();
        transaction
            .open_table(TRANSACTIONS)
            .unwrap()
            .insert(id.allocation(), bytes.as_slice())
            .unwrap();
        transaction.commit().unwrap();
        assert!(matches!(
            store.commit_status(id, &control),
            Err(VersionError::InvalidEncoding(_))
        ));
        assert!(matches!(
            store.reclaim_transaction_receipts(&control),
            Err(VersionError::InvalidEncoding(_))
        ));
        assert_eq!(count(&store), 1);
    }
    for bytes in [vec![], 0_u64.to_be_bytes().to_vec(), vec![1]] {
        let store = memory();
        let transaction = physical_writer(&store.database).unwrap();
        transaction
            .open_table(METADATA)
            .unwrap()
            .insert("receipt_limit", bytes.as_slice())
            .unwrap();
        transaction.commit().unwrap();
        assert!(matches!(
            RedbRecordStore::new(Arc::clone(&store.database)),
            Err(VersionError::InvalidEncoding(_))
        ));
        let read = store.database.begin_read().unwrap();
        assert_eq!(
            read.open_table(METADATA)
                .unwrap()
                .get("receipt_limit")
                .unwrap()
                .unwrap()
                .value(),
            bytes
        );
    }
}

#[test]
fn uncertain_acknowledgement_and_collection_reopen_with_one_complete_receipt_state() {
    for collect in [false, true] {
        let backend = FaultBackend::default();
        let control = StorageReadControl::with_limit(1 << 20);
        let receipt = {
            let store = RedbRecordStore::new(Arc::new(
                Database::builder()
                    .create_with_backend(backend.clone())
                    .unwrap(),
            ))
            .unwrap();
            let id = store.allocate_transaction(&control).unwrap();
            let receipt = store.commit(id, &empty(&control), &control).unwrap();
            if collect {
                store
                    .acknowledge_transaction(ReceiptAcknowledgement::Committed(receipt), &control)
                    .unwrap();
                // Materialize the unchanged SSI checkpoint before injecting the receipt-deletion sync failure.
                store.recover_serializable_participants(&control).unwrap();
            }
            backend.fail_sync.store(true, Ordering::Relaxed);
            let result = if collect {
                store.reclaim_transaction_receipts(&control).map(|_| ())
            } else {
                store.acknowledge_transaction(ReceiptAcknowledgement::Committed(receipt), &control)
            };
            assert!(result.is_err());
            assert!(!backend.fail_sync.load(Ordering::Relaxed));
            receipt
        };
        let store = RedbRecordStore::new(Arc::new(
            Database::builder().create_with_backend(backend).unwrap(),
        ))
        .unwrap();
        assert!(matches!(
            store.commit_status(receipt.transaction, &control).unwrap(),
            CommitStatus::Unknown | CommitStatus::Committed(_)
        ));
        store
            .acknowledge_transaction(ReceiptAcknowledgement::Committed(receipt), &control)
            .unwrap();
        store.reclaim_transaction_receipts(&control).unwrap();
        assert_eq!(
            store.commit_status(receipt.transaction, &control).unwrap(),
            CommitStatus::Unknown
        );
        assert!(
            store.allocate_transaction(&control).unwrap().allocation()
                > receipt.transaction.allocation()
        );
    }
}

#[test]
fn failed_managed_allocation_sync_releases_its_owner_and_durable_pending_receipt() {
    let backend = FaultBackend::default();
    let control = StorageReadControl::with_limit(1 << 20);
    {
        let store = RedbRecordStore::new(Arc::new(
            Database::builder()
                .create_with_backend(backend.clone())
                .unwrap(),
        ))
        .unwrap();
        backend.fail_sync.store(true, Ordering::Relaxed);
        assert!(store.allocate_managed_transaction(&control).is_err());
        assert!(!backend.fail_sync.load(Ordering::Relaxed));
    }
    assert_eq!(control.memory().used(), 0);
    let store = RedbRecordStore::new(Arc::new(
        Database::builder().create_with_backend(backend).unwrap(),
    ))
    .unwrap();
    assert!(count(&store) <= 1);
    store.reclaim_transaction_receipts(&control).unwrap();
    assert_eq!(count(&store), 0);
    let next = store.allocate_managed_transaction(&control).unwrap();
    assert_eq!(
        store.commit_status(next.transaction(), &control).unwrap(),
        CommitStatus::Pending
    );
}

#[test]
fn failed_receipt_format_upgrade_reopens_as_one_complete_format_with_manual_ownership() {
    let backend = FaultBackend::default();
    let control = StorageReadControl::with_limit(1 << 20);
    let id = {
        let database = Arc::new(
            Database::builder()
                .create_with_backend(backend.clone())
                .unwrap(),
        );
        let store = RedbRecordStore::new(Arc::clone(&database)).unwrap();
        let id = store.allocate_transaction(&control).unwrap();
        let transaction = physical_writer(&database).unwrap();
        {
            let mut metadata = transaction.open_table(METADATA).unwrap();
            metadata
                .insert("format", 42_u64.to_be_bytes().as_slice())
                .unwrap();
            metadata.remove("receipt_limit").unwrap();
        }
        transaction.commit().unwrap();
        drop(store);
        backend.fail_sync.store(true, Ordering::Relaxed);
        assert!(RedbRecordStore::new(database).is_err());
        assert!(!backend.fail_sync.load(Ordering::Relaxed));
        id
    };
    let database = Arc::new(Database::builder().create_with_backend(backend).unwrap());
    {
        let read = database.begin_read().unwrap();
        let metadata = read.open_table(METADATA).unwrap();
        match codec::read_u64(&metadata, "format").unwrap() {
            42 => assert!(metadata.get("receipt_limit").unwrap().is_none()),
            46 => assert_eq!(
                codec::receipt_limit(&metadata).unwrap(),
                DEFAULT_RECEIPT_RETENTION_LIMIT
            ),
            other => panic!("incomplete receipt format: {other}"),
        }
        assert_eq!(
            read.open_table(TRANSACTIONS)
                .unwrap()
                .get(id.allocation())
                .unwrap()
                .unwrap()
                .value(),
            [0]
        );
    }
    let store = RedbRecordStore::new(database).unwrap();
    assert_eq!(store.reclaim_transaction_receipts(&control).unwrap(), 0);
    assert_eq!(
        store.commit_status(id, &control).unwrap(),
        CommitStatus::Pending
    );
}
