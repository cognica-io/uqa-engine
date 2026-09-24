//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Manual resolution rights, managed owner death and durable SSI references bound receipts.

use super::*;
use uqa_storage::mvcc::{ReceiptAcknowledgement, SerializableCoordinator, SerializableStatus};

#[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
mod process;

fn open(path: &std::path::Path, mode: usize) -> ManagedConnection {
    if mode == 4 {
        ManagedConnection::open_in_memory().unwrap()
    } else {
        super::reclamation::open(path, mode)
    }
}

fn records(
    connection: &ManagedConnection,
    native: bool,
    control: &StorageReadControl,
) -> SQLiteRecordStore {
    if native {
        crate::Catalog::open(connection.clone()).unwrap();
        SQLiteRecordStore::for_native(connection, control).unwrap()
    } else {
        SQLiteRecordStore::new(connection).unwrap()
    }
}

fn empty(control: &StorageReadControl) -> PreparedRecordCommit {
    PreparedRecordCommit::new(&[], control).unwrap()
}

#[test]
fn manual_receipts_require_exact_acknowledgement_and_keep_the_durable_limit() {
    for mode in 0..5 {
        for native in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("receipt.db");
            let control = control();
            let connection = open(&path, mode);
            let store = records(&connection, native, &control);
            assert_eq!(
                store
                    .with(|connection| Ok(codec::header(connection, store.identity)?.receipt_limit))
                    .unwrap(),
                uqa_storage::mvcc::DEFAULT_RECEIPT_RETENTION_LIMIT
            );
            store.set_receipt_retention_limit(2, &control).unwrap();
            let pending = store.allocate_transaction(&control).unwrap();
            let id = store.allocate_transaction(&control).unwrap();
            let receipt = store.commit(id, &empty(&control), &control).unwrap();
            assert!(store.set_receipt_retention_limit(0, &control).is_err());
            store.set_receipt_retention_limit(1, &control).unwrap();
            assert!(matches!(
                store.allocate_transaction(&control),
                Err(VersionError::ReceiptRetentionExhausted { limit: 1 })
            ));
            assert_eq!(
                store.commit_status(id, &control).unwrap(),
                CommitStatus::Committed(receipt)
            );
            store.set_receipt_retention_limit(2, &control).unwrap();
            assert!(matches!(
                store.allocate_transaction(&control),
                Err(VersionError::ReceiptRetentionExhausted { limit: 2 })
            ));
            assert!(store
                .acknowledge_transaction(ReceiptAcknowledgement::Aborted(pending), &control)
                .is_err());
            assert!(store
                .acknowledge_transaction(
                    ReceiptAcknowledgement::Committed(uqa_storage::mvcc::CommitReceipt {
                        fingerprint: [91; 32],
                        ..receipt
                    }),
                    &control
                )
                .is_err());
            assert_eq!(store.reclaim_transaction_receipts(&control).unwrap(), 0);
            store
                .acknowledge_transaction(ReceiptAcknowledgement::Committed(receipt), &control)
                .unwrap();
            assert_eq!(store.reclaim_transaction_receipts(&control).unwrap(), 1);
            assert_eq!(
                store.commit_status(id, &control).unwrap(),
                CommitStatus::Unknown
            );
            assert_eq!(
                store.commit_status(pending, &control).unwrap(),
                CommitStatus::Pending
            );
            store
                .acknowledge_transaction(ReceiptAcknowledgement::Committed(receipt), &control)
                .unwrap();
            assert_eq!(
                store.allocate_transaction(&control).unwrap().allocation(),
                id.allocation() + 1
            );
            assert!(matches!(
                store.allocate_transaction(&control),
                Err(VersionError::ReceiptRetentionExhausted { limit: 2 })
            ));
            if mode != 4 {
                drop((store, connection));
                let connection = open(&path, mode);
                let store = if native {
                    SQLiteRecordStore::for_native(&connection, &control).unwrap()
                } else {
                    records(&connection, false, &control)
                };
                assert_eq!(
                    store.commit_status(id, &control).unwrap(),
                    CommitStatus::Unknown
                );
                assert_eq!(
                    store.commit_status(pending, &control).unwrap(),
                    CommitStatus::Pending
                );
                assert!(matches!(
                    store.allocate_transaction(&control),
                    Err(VersionError::ReceiptRetentionExhausted { limit: 2 })
                ));
            }
        }
    }
}

#[test]
fn managed_live_owners_survive_reclamation_and_dead_owners_release_only_their_receipts() {
    for mode in 0..5 {
        for native in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("managed.db");
            let control = control();
            let connection = open(&path, mode);
            let store = records(&connection, native, &control);
            let manual = store.allocate_transaction(&control).unwrap();
            let pending = store.allocate_managed_transaction(&control).unwrap();
            let committed = store.allocate_managed_transaction(&control).unwrap();
            let id = committed.transaction();
            let receipt = store.commit(id, &empty(&control), &control).unwrap();
            let snapshot = store.snapshot(&control).unwrap();
            assert_eq!(store.reclaim_transaction_receipts(&control).unwrap(), 0);
            assert_eq!(
                store.commit_status(id, &control).unwrap(),
                CommitStatus::Committed(receipt)
            );
            drop(committed);
            assert_eq!(store.reclaim_transaction_receipts(&control).unwrap(), 1);
            assert_eq!(
                store.commit_status(id, &control).unwrap(),
                CommitStatus::Unknown
            );
            let pending_id = pending.transaction();
            drop(pending);
            assert_eq!(store.reclaim_transaction_receipts(&control).unwrap(), 1);
            assert_eq!(
                store.commit_status(pending_id, &control).unwrap(),
                CommitStatus::Unknown
            );
            assert_eq!(
                store.commit_status(manual, &control).unwrap(),
                CommitStatus::Pending
            );
            assert_eq!(snapshot.sequence(), receipt.sequence);
        }
    }
}

#[test]
fn acknowledged_receipts_outlive_terminal_leases_and_overlapping_serializable_history() {
    let control = control();
    let connection = ManagedConnection::open_in_memory().unwrap();
    let store = SQLiteRecordStore::new(&connection).unwrap();
    let (older, ()) = store
        .admit_serializable(false, &control, || Ok(()))
        .unwrap();
    let (writer, ()) = store
        .admit_serializable(false, &control, || Ok(()))
        .unwrap();
    let owner = store.allocate_managed_transaction(&control).unwrap();
    let id = owner.transaction();
    let prepared = empty(&control);
    let mut publication = None;
    store
        .with_serializable_admission(&control, &mut |graph, _| {
            publication = Some(graph.prepare_publication(
                writer.id(),
                id,
                prepared.fingerprint(),
                &control,
            )?);
            Ok(())
        })
        .unwrap();
    let receipt = store.commit(id, &prepared, &control).unwrap();
    store
        .with_serializable_admission(&control, &mut |graph, _| {
            graph.resolve_publication(publication.unwrap(), CommitStatus::Committed(receipt))?;
            Ok(())
        })
        .unwrap();
    store
        .acknowledge_transaction(ReceiptAcknowledgement::Committed(receipt), &control)
        .unwrap();
    assert_eq!(store.reclaim_transaction_receipts(&control).unwrap(), 0);
    drop((owner, writer));
    assert_eq!(store.reclaim_transaction_receipts(&control).unwrap(), 0);
    store
        .with_serializable_admission(&control, &mut |graph, _| {
            assert_eq!(graph.status(older.id())?, SerializableStatus::Active);
            assert!(graph.retains_transaction_receipt(id));
            graph.rollback(older.id())
        })
        .unwrap();
    drop(older);
    assert_eq!(store.reclaim_transaction_receipts(&control).unwrap(), 1);
    assert_eq!(
        store.commit_status(id, &control).unwrap(),
        CommitStatus::Unknown
    );
}

#[test]
fn cancelled_or_exhausted_collection_keeps_acknowledgements_for_a_bounded_retry() {
    let control = control();
    let connection = ManagedConnection::open_in_memory().unwrap();
    let store = SQLiteRecordStore::new(&connection).unwrap();
    let mut ids = Vec::new();
    for _ in 0..257 {
        let id = store.allocate_transaction(&control).unwrap();
        assert_eq!(store.abort(id, &control).unwrap(), CommitStatus::Aborted);
        store
            .acknowledge_transaction(ReceiptAcknowledgement::Aborted(id), &control)
            .unwrap();
        ids.push(id);
    }
    let cancelled = StorageReadControl::with_limit(1 << 20);
    cancelled.cancellation().cancel();
    assert!(store.reclaim_transaction_receipts(&cancelled).is_err());
    assert!(store
        .reclaim_transaction_receipts(&StorageReadControl::with_limit(0))
        .is_err());
    assert_eq!(
        store.commit_status(ids[0], &control).unwrap(),
        CommitStatus::Aborted
    );
    assert_eq!(store.reclaim_transaction_receipts(&control).unwrap(), 256);
    assert_eq!(
        store.commit_status(ids[256], &control).unwrap(),
        CommitStatus::Aborted
    );
    assert_eq!(store.reclaim_transaction_receipts(&control).unwrap(), 1);
    assert_eq!(store.reclaim_transaction_receipts(&control).unwrap(), 0);
}

#[test]
fn managed_allocation_failure_does_not_consume_a_pending_identity() {
    let control = control();
    let connection = ManagedConnection::open_in_memory().unwrap();
    let store = SQLiteRecordStore::new(&connection).unwrap();
    let first = store.allocate_transaction(&control).unwrap();
    assert!(store
        .allocate_managed_transaction(&StorageReadControl::with_limit(0))
        .is_err());
    let next = store.allocate_managed_transaction(&control).unwrap();
    assert_eq!(next.transaction().allocation(), first.allocation() + 1);
    assert_eq!(
        store.commit_status(next.transaction(), &control).unwrap(),
        CommitStatus::Pending
    );
}

#[test]
fn managed_abandonment_retains_prepared_ssi_evidence_until_its_last_actor_releases() {
    let control = control();
    let connection = ManagedConnection::open_in_memory().unwrap();
    let store = SQLiteRecordStore::new(&connection).unwrap();
    let (actor, ()) = store
        .admit_serializable(false, &control, || Ok(()))
        .unwrap();
    let owner = store.allocate_managed_transaction(&control).unwrap();
    let id = owner.transaction();
    store
        .with_serializable_admission(&control, &mut |graph, _| {
            graph.prepare_publication(actor.id(), id, [7; 32], &control)?;
            Ok(())
        })
        .unwrap();
    drop(owner);
    assert_eq!(store.reclaim_transaction_receipts(&control).unwrap(), 0);
    assert_eq!(
        store.commit_status(id, &control).unwrap(),
        CommitStatus::Aborted
    );
    assert_eq!(store.reclaim_transaction_receipts(&control).unwrap(), 0);
    store
        .with_serializable_admission(&control, &mut |graph, _| {
            assert_eq!(graph.status(actor.id())?, SerializableStatus::Aborted);
            assert!(graph.retains_transaction_receipt(id));
            Ok(())
        })
        .unwrap();
    drop(actor);
    assert_eq!(store.reclaim_transaction_receipts(&control).unwrap(), 1);
}

#[test]
fn independent_allocators_cannot_exceed_the_shared_receipt_limit() {
    for mode in [0, 2] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("race.db");
        let first = SQLiteRecordStore::new(&open(&path, mode)).unwrap();
        let second = SQLiteRecordStore::new(&open(&path, mode)).unwrap();
        let control = control();
        first.set_receipt_retention_limit(1, &control).unwrap();
        let barrier = std::sync::Barrier::new(2);
        let outcomes = std::thread::scope(|scope| {
            let left = scope.spawn(|| {
                barrier.wait();
                first.allocate_transaction(&control)
            });
            let right = scope.spawn(|| {
                barrier.wait();
                second.allocate_transaction(&control)
            });
            [left.join().unwrap(), right.join().unwrap()]
        });
        assert_eq!(outcomes.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            outcomes
                .iter()
                .filter(|result| matches!(
                    result,
                    Err(VersionError::ReceiptRetentionExhausted { limit: 1 })
                ))
                .count(),
            1
        );
    }
}

#[test]
fn collection_rejects_receipts_above_the_watermark_without_deleting_earlier_candidates() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let store = SQLiteRecordStore::new(&connection).unwrap();
    let control = control();
    let id = store.allocate_transaction(&control).unwrap();
    let receipt = store.commit(id, &empty(&control), &control).unwrap();
    store
        .acknowledge_transaction(ReceiptAcknowledgement::Committed(receipt), &control)
        .unwrap();
    let owner = store.allocate_managed_transaction(&control).unwrap();
    let abandoned = owner.transaction();
    drop(owner);
    store
        .with(|sqlite| {
            let _permit = schema::WritePermit::acquire(sqlite)?;
            sqlite.execute(
                "UPDATE _uqa_mvcc_metadata SET allocated = ?1",
                [id.allocation().to_be_bytes().as_slice()],
            )?;
            Ok(())
        })
        .unwrap();
    assert!(matches!(
        store.reclaim_transaction_receipts(&control),
        Err(VersionError::InvalidEncoding(
            "receipt exceeds allocation watermark"
        ))
    ));
    assert_eq!(
        store.commit_status(id, &control).unwrap(),
        CommitStatus::Committed(receipt)
    );
    assert_eq!(
        store.commit_status(abandoned, &control).unwrap(),
        CommitStatus::Pending
    );
}
