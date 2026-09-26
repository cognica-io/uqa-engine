//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Receipt ownership, admission limits and SSI references remain independent of history cleanup.

mod failure;
mod process;

use super::*;
use uqa_storage::mvcc::{ReceiptAcknowledgement, RecordWrite, SerializableCoordinator};

fn memory() -> RedbRecordStore {
    RedbRecordStore::new(Arc::new(
        Database::builder()
            .create_with_backend(InMemoryBackend::new())
            .unwrap(),
    ))
    .unwrap()
}

fn empty(control: &StorageReadControl) -> PreparedRecordCommit {
    PreparedRecordCommit::new(&[], control).unwrap()
}

fn count(store: &RedbRecordStore) -> u64 {
    store
        .database
        .begin_read()
        .unwrap()
        .open_table(TRANSACTIONS)
        .unwrap()
        .len()
        .unwrap()
}

#[test]
fn value_format_upgrade_preserves_receipt_capacity_acknowledgement_and_live_ownership() {
    for predecessor in [43_u64, 44, 45, 46, 47, 48, 49, 50] {
        let store = memory();
        let control = StorageReadControl::with_limit(1 << 20);
        store.set_receipt_retention_limit(7, &control).unwrap();
        let owner = store.allocate_managed_transaction(&control).unwrap();
        let id = store.allocate_transaction(&control).unwrap();
        let receipt = store.commit(id, &empty(&control), &control).unwrap();
        store
            .acknowledge_transaction(ReceiptAcknowledgement::Committed(receipt), &control)
            .unwrap();
        let transaction = physical_writer(&store.database).unwrap();
        transaction
            .open_table(METADATA)
            .unwrap()
            .insert("format", predecessor.to_be_bytes().as_slice())
            .unwrap();
        transaction.commit().unwrap();
        let upgraded = RedbRecordStore::new(Arc::clone(&store.database)).unwrap();
        let read = upgraded.database.begin_read().unwrap();
        assert_eq!(
            codec::receipt_limit(&read.open_table(METADATA).unwrap()).unwrap(),
            7
        );
        drop(read);
        assert_eq!(
            upgraded.commit_status(id, &control).unwrap(),
            CommitStatus::Committed(receipt)
        );
        assert_eq!(upgraded.reclaim_transaction_receipts(&control).unwrap(), 1);
        assert_eq!(
            upgraded
                .commit_status(owner.transaction(), &control)
                .unwrap(),
            CommitStatus::Pending
        );
        drop(owner);
        assert_eq!(upgraded.reclaim_transaction_receipts(&control).unwrap(), 1);
    }
}

#[test]
fn manual_receipts_require_exact_acknowledgement_and_never_expire_by_owner_absence() {
    let store = memory();
    let control = StorageReadControl::with_limit(1 << 20);
    store.set_receipt_retention_limit(2, &control).unwrap();
    let pending = store.allocate_transaction(&control).unwrap();
    let committed = store.allocate_transaction(&control).unwrap();
    let prepared = empty(&control);
    let receipt = store.commit(committed, &prepared, &control).unwrap();
    assert!(matches!(
        store.allocate_transaction(&control),
        Err(VersionError::ReceiptRetentionExhausted { limit: 2 })
    ));
    assert_eq!(store.reclaim_transaction_receipts(&control).unwrap(), 0);
    assert!(matches!(
        store.acknowledge_transaction(ReceiptAcknowledgement::Aborted(pending), &control),
        Err(VersionError::TransactionSealed)
    ));
    for altered in [
        CommitReceipt {
            fingerprint: [1; 32],
            ..receipt
        },
        CommitReceipt {
            sequence: CommitSequence::from_u64(1),
            ..receipt
        },
    ] {
        assert!(
            matches!(store.acknowledge_transaction(ReceiptAcknowledgement::Committed(altered), &control), Err(VersionError::AlreadyCommitted(actual)) if actual == receipt)
        );
    }
    assert!(
        matches!(store.acknowledge_transaction(ReceiptAcknowledgement::Aborted(committed), &control), Err(VersionError::AlreadyCommitted(actual)) if actual == receipt)
    );
    store
        .acknowledge_transaction(ReceiptAcknowledgement::Committed(receipt), &control)
        .unwrap();
    assert_eq!(
        store.commit(committed, &prepared, &control).unwrap(),
        receipt
    );
    assert_eq!(
        store.abort(committed, &control).unwrap(),
        CommitStatus::Committed(receipt)
    );
    assert_eq!(store.reclaim_transaction_receipts(&control).unwrap(), 1);
    assert_eq!(
        store.commit_status(committed, &control).unwrap(),
        CommitStatus::Unknown
    );
    store
        .acknowledge_transaction(ReceiptAcknowledgement::Committed(receipt), &control)
        .unwrap();
    assert!(matches!(
        store.commit(committed, &prepared, &control),
        Err(CommitFailure::Rejected(VersionError::UnknownTransaction))
    ));
    store.abort(pending, &control).unwrap();
    store
        .acknowledge_transaction(ReceiptAcknowledgement::Aborted(pending), &control)
        .unwrap();
    assert_eq!(store.reclaim_transaction_receipts(&control).unwrap(), 1);
    store
        .acknowledge_transaction(ReceiptAcknowledgement::Aborted(pending), &control)
        .unwrap();
    let next = store.allocate_transaction(&control).unwrap();
    assert!(next.allocation() > committed.allocation());
    let future = StorageTransactionId::new(store.identity, next.allocation() + 1).unwrap();
    assert!(matches!(
        store.acknowledge_transaction(ReceiptAcknowledgement::Aborted(future), &control),
        Err(VersionError::UnknownTransaction)
    ));
    assert!(matches!(
        memory().acknowledge_transaction(ReceiptAcknowledgement::Aborted(pending), &control),
        Err(VersionError::WrongDatabase)
    ));
    assert_eq!(
        store.snapshot(&control).unwrap().sequence(),
        CommitSequence::INITIAL
    );
}

#[test]
fn managed_owners_survive_adapter_churn_and_release_pending_committed_and_aborted_receipts() {
    let store = memory();
    let database = Arc::downgrade(&store.database);
    let state = Arc::downgrade(&store.receipts);
    let control = StorageReadControl::with_limit(1 << 20);
    let pending = store.allocate_managed_transaction(&control).unwrap();
    let committed = store.allocate_managed_transaction(&control).unwrap();
    let aborted = store.allocate_managed_transaction(&control).unwrap();
    let manual = store.allocate_transaction(&control).unwrap();
    let prepared = PreparedRecordCommit::new(
        &[RecordWrite {
            key: b"kept",
            expected: None,
            value: Some(b"durable"),
        }],
        &control,
    )
    .unwrap();
    let receipt = store
        .commit(committed.transaction(), &prepared, &control)
        .unwrap();
    store.abort(aborted.transaction(), &control).unwrap();
    drop(store);
    let peer = RedbRecordStore::new(database.upgrade().unwrap()).unwrap();
    assert!(Arc::ptr_eq(&state.upgrade().unwrap(), &peer.receipts));
    assert!(!Arc::ptr_eq(&peer.serializable, &peer.receipts));
    assert_eq!(peer.reclaim_transaction_receipts(&control).unwrap(), 0);
    let ids = [
        pending.transaction(),
        committed.transaction(),
        aborted.transaction(),
    ];
    drop((pending, committed, aborted));
    assert_eq!(peer.reclaim_transaction_receipts(&control).unwrap(), 3);
    for id in ids {
        assert_eq!(
            peer.commit_status(id, &control).unwrap(),
            CommitStatus::Unknown
        );
    }
    assert_eq!(
        peer.commit_status(manual, &control).unwrap(),
        CommitStatus::Pending
    );
    let snapshot = peer.snapshot(&control).unwrap();
    assert_eq!(snapshot.sequence(), receipt.sequence);
    assert_eq!(
        snapshot
            .get(b"kept", &control)
            .unwrap()
            .unwrap()
            .value()
            .map(|value| &***value),
        Some(b"durable".as_slice())
    );
    drop((snapshot, peer, prepared));
    assert!(state.upgrade().is_none());
    assert!(database.upgrade().is_none());
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn receipt_admission_limit_is_atomic_across_independent_adapters() {
    let store = memory();
    let peer = RedbRecordStore::new(Arc::clone(&store.database)).unwrap();
    let control = StorageReadControl::with_limit(1 << 20);
    store.set_receipt_retention_limit(1, &control).unwrap();
    let barrier = std::sync::Barrier::new(2);
    let results = std::thread::scope(|scope| {
        let a = scope.spawn(|| {
            barrier.wait();
            store.allocate_transaction(&control)
        });
        barrier.wait();
        let b = peer.allocate_transaction(&control);
        [a.join().unwrap(), b]
    });
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(
                result,
                Err(VersionError::ReceiptRetentionExhausted { limit: 1 })
            ))
            .count(),
        1
    );
    assert_eq!(count(&store), 1);
    assert!(store.set_receipt_retention_limit(0, &control).is_err());
    let reopened = RedbRecordStore::new(Arc::clone(&store.database)).unwrap();
    assert!(matches!(
        reopened.allocate_transaction(&control),
        Err(VersionError::ReceiptRetentionExhausted { limit: 1 })
    ));
}

#[test]
fn collection_is_bounded_and_protected_prefixes_do_not_starve_later_receipts() {
    let store = memory();
    let control = StorageReadControl::with_limit(1 << 22);
    let prepared = empty(&control);
    let mut participants = Vec::new();
    let mut publications = Vec::new();
    store
        .with_serializable_admission(&control, &mut |graph, leases| {
            for _ in 0..256 {
                let participant =
                    graph.admit_with_lease(false, &control, |id| leases.retain(id, &control))?;
                let id = store.allocate_transaction(&control)?;
                let publication = graph.prepare_publication(
                    participant.id(),
                    id,
                    prepared.fingerprint(),
                    &control,
                )?;
                publications.push(publication);
                participants.push(participant);
            }
            Ok(())
        })
        .unwrap();
    store
        .with_serializable_admission(&control, &mut |graph, _| {
            for publication in &publications {
                let receipt = store
                    .commit(publication.transaction(), &prepared, &control)
                    .unwrap();
                graph.resolve_publication(*publication, CommitStatus::Committed(receipt))?;
                store.acknowledge_transaction(
                    ReceiptAcknowledgement::Committed(receipt),
                    &control,
                )?;
            }
            Ok(())
        })
        .unwrap();
    for _ in 0..257 {
        let id = store.allocate_transaction(&control).unwrap();
        store.abort(id, &control).unwrap();
        store
            .acknowledge_transaction(ReceiptAcknowledgement::Aborted(id), &control)
            .unwrap();
    }
    assert_eq!(store.reclaim_transaction_receipts(&control).unwrap(), 256);
    assert_eq!(count(&store), 257);
    assert_eq!(store.reclaim_transaction_receipts(&control).unwrap(), 1);
    assert_eq!(store.reclaim_transaction_receipts(&control).unwrap(), 0);
    drop(participants);
    assert_eq!(store.reclaim_transaction_receipts(&control).unwrap(), 256);
    assert_eq!(count(&store), 0);
    drop(prepared);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn acknowledged_commits_survive_overlapping_history_after_their_terminal_owner_drops() {
    let store = memory();
    let control = StorageReadControl::with_limit(1 << 20);
    let (reader, reader_view) = store.admit_serializable_snapshot(true, &control).unwrap();
    let (publisher, publisher_view) = store.admit_serializable_snapshot(false, &control).unwrap();
    let allocation = store.allocate_managed_transaction(&control).unwrap();
    let prepared = empty(&control);
    let mut publication = None;
    store
        .with_serializable_admission(&control, &mut |graph, _| {
            publication = Some(graph.prepare_publication(
                publisher.id(),
                allocation.transaction(),
                prepared.fingerprint(),
                &control,
            )?);
            Ok(())
        })
        .unwrap();
    let publication = publication.unwrap();
    let receipt = store
        .commit(allocation.transaction(), &prepared, &control)
        .unwrap();
    store
        .with_serializable_admission(&control, &mut |graph, _| {
            graph.resolve_publication(publication, CommitStatus::Committed(receipt))?;
            Ok(())
        })
        .unwrap();
    store
        .acknowledge_transaction(ReceiptAcknowledgement::Committed(receipt), &control)
        .unwrap();
    drop((publisher, publisher_view, allocation));
    assert_eq!(store.reclaim_transaction_receipts(&control).unwrap(), 0);
    assert_eq!(
        store.commit_status(receipt.transaction, &control).unwrap(),
        CommitStatus::Committed(receipt)
    );
    drop((reader, reader_view));
    assert_eq!(store.reclaim_transaction_receipts(&control).unwrap(), 1);
}

#[test]
fn dead_managed_pending_owner_is_aborted_but_its_live_ssi_publication_remains_resolvable() {
    let store = memory();
    let control = StorageReadControl::with_limit(1 << 20);
    let (participant, snapshot) = store.admit_serializable_snapshot(false, &control).unwrap();
    let allocation = store.allocate_managed_transaction(&control).unwrap();
    let id = allocation.transaction();
    let prepared = empty(&control);
    let mut publication = None;
    store
        .with_serializable_admission(&control, &mut |graph, _| {
            publication = Some(graph.prepare_publication(
                participant.id(),
                id,
                prepared.fingerprint(),
                &control,
            )?);
            Ok(())
        })
        .unwrap();
    drop(allocation);
    assert_eq!(store.reclaim_transaction_receipts(&control).unwrap(), 0);
    assert_eq!(
        store.commit_status(id, &control).unwrap(),
        CommitStatus::Aborted
    );
    store
        .with_serializable_admission(&control, &mut |graph, _| {
            assert_eq!(
                graph.resolve_publication(
                    publication.unwrap(),
                    store.commit_status(id, &control)?
                )?,
                CommitStatus::Aborted
            );
            Ok(())
        })
        .unwrap();
    assert_eq!(store.reclaim_transaction_receipts(&control).unwrap(), 0);
    drop((participant, snapshot));
    assert_eq!(store.reclaim_transaction_receipts(&control).unwrap(), 1);
}

#[test]
fn legacy_migration_preserves_manual_receipts_and_restore_preserves_the_limit() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("receipts.redb");
    let control = StorageReadControl::with_limit(1 << 20);
    let (id, request) = {
        let store = RedbRecordStore::new(Arc::new(Database::create(&path).unwrap())).unwrap();
        let id = store.allocate_transaction(&control).unwrap();
        let transaction = physical_writer(&store.database).unwrap();
        {
            let mut metadata = transaction.open_table(METADATA).unwrap();
            metadata
                .insert("format", 42_u64.to_be_bytes().as_slice())
                .unwrap();
            metadata.remove("receipt_limit").unwrap();
        }
        transaction.commit().unwrap();
        (
            id,
            uqa_storage::mvcc::DatabaseRestore::new(store.identity).unwrap(),
        )
    };
    {
        let store = RedbRecordStore::new(Arc::new(Database::open(&path).unwrap())).unwrap();
        assert_eq!(
            store.commit_status(id, &control).unwrap(),
            CommitStatus::Pending
        );
        assert_eq!(store.reclaim_transaction_receipts(&control).unwrap(), 0);
        let read = store.database.begin_read().unwrap();
        assert_eq!(
            codec::receipt_limit(&read.open_table(METADATA).unwrap()).unwrap(),
            DEFAULT_RECEIPT_RETENTION_LIMIT
        );
        drop(read);
        store.set_receipt_retention_limit(1, &control).unwrap();
    }
    let store =
        crate::mvcc::restore::open(Database::open(&path).unwrap(), request, &control).unwrap();
    assert_eq!(count(&store), 0);
    assert!(matches!(
        store.commit_status(id, &control),
        Err(VersionError::WrongDatabase)
    ));
    let next = store.allocate_transaction(&control).unwrap();
    assert!(next.allocation() > id.allocation());
    assert!(matches!(
        store.allocate_transaction(&control),
        Err(VersionError::ReceiptRetentionExhausted { limit: 1 })
    ));
}
