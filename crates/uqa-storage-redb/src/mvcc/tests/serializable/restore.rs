//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Closed backups start a distinct history without relabeling receipts or losing committed records.

use super::*;
use uqa_storage::mvcc::{DatabaseRestore, IdentifierRequest, SerializableTransactionId};

struct Backup {
    request: DatabaseRestore,
    old_actor: SerializableTransactionId,
    receipt: CommitReceipt,
    pending: StorageTransactionId,
    aborted: StorageTransactionId,
    sequence: CommitSequence,
}

impl Backup {
    fn capture(path: &std::path::Path, control: &StorageReadControl, reclaim: bool) -> Self {
        let store = RedbRecordStore::new(Arc::new(Database::create(path).unwrap())).unwrap();
        let reader = actor(&store, control);
        let publisher = actor(&store, control);
        let (publication, prepared) = prepare(&store, &publisher, b"committed", control);
        let receipt = graph(&store, control, |graph| {
            let receipt = store
                .commit(publication.transaction(), &prepared, control)
                .unwrap();
            graph.resolve_publication(publication, CommitStatus::Committed(receipt))?;
            Ok(receipt)
        })
        .unwrap();
        let old_actor = reader.id();
        let pending = store.allocate_transaction(control).unwrap();
        let aborted = store.allocate_transaction(control).unwrap();
        store.abort(aborted, control).unwrap();
        store
            .allocate_identifiers(b"rows", IdentifierRequest::Observe(77), control)
            .unwrap();
        let sequence = store.snapshot(control).unwrap().sequence();
        drop((reader, publisher));
        if reclaim {
            store.recover_serializable_participants(control).unwrap();
            store.reclaim_versions(control).unwrap();
        }
        Self {
            request: DatabaseRestore::new(store.identity).unwrap(),
            old_actor,
            receipt,
            pending,
            aborted,
            sequence,
        }
    }
}

fn assert_restored_values(
    store: &RedbRecordStore,
    sequence: CommitSequence,
    control: &StorageReadControl,
) {
    assert_eq!(
        store.identifier_watermark(b"rows", control).unwrap(),
        Some(77)
    );
    let view = store.snapshot(control).unwrap();
    assert_eq!(view.sequence(), sequence);
    assert_eq!(
        view.get(b"committed", control)
            .unwrap()
            .unwrap()
            .value()
            .map(|v| &***v),
        Some(b"durable".as_slice())
    );
}

fn assert_original_outcomes(
    store: &RedbRecordStore,
    receipt: CommitReceipt,
    pending: StorageTransactionId,
    aborted: StorageTransactionId,
    control: &StorageReadControl,
) {
    assert_eq!(
        store.commit_status(receipt.transaction, control).unwrap(),
        CommitStatus::Committed(receipt)
    );
    assert_eq!(
        store.commit_status(pending, control).unwrap(),
        CommitStatus::Pending
    );
    assert_eq!(
        store.commit_status(aborted, control).unwrap(),
        CommitStatus::Aborted
    );
}

#[test]
fn restoring_retained_and_reclaimed_checkpoints_preserves_data_but_retires_old_outcomes() {
    for reclaim in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("source.redb");
        let backup = directory.path().join("backup.redb");
        let control = StorageReadControl::with_limit(1 << 20);
        let Backup {
            request,
            old_actor,
            receipt,
            pending,
            aborted,
            sequence,
        } = Backup::capture(&path, &control, reclaim);
        assert_eq!(control.memory().used(), 0);
        std::fs::copy(&path, &backup).unwrap();
        {
            // Ordinary reopen of the complete copied history must preserve receipt resolution.
            let store = RedbRecordStore::new(Arc::new(Database::open(&backup).unwrap())).unwrap();
            assert_eq!(store.identity, request.source());
            assert_original_outcomes(&store, receipt, pending, aborted, &control);
        }
        let (new_actor, new_receipt) = {
            let store =
                crate::mvcc::restore::open(Database::open(&backup).unwrap(), request, &control)
                    .unwrap();
            assert_eq!(store.identity, request.target());
            for old in [receipt.transaction, pending, aborted] {
                assert!(matches!(
                    store.commit_status(old, &control),
                    Err(VersionError::WrongDatabase)
                ));
                let relabeled =
                    StorageTransactionId::new(store.identity, old.allocation()).unwrap();
                assert_eq!(
                    store.commit_status(relabeled, &control).unwrap(),
                    CommitStatus::Unknown
                );
            }
            assert_restored_values(&store, sequence, &control);
            let next = actor(&store, &control);
            assert_ne!(next.id().coordinator(), old_actor.coordinator());
            graph(&store, &control, |graph| {
                assert!(matches!(
                    graph.check_active(old_actor),
                    Err(VersionError::WrongDatabase)
                ));
                Ok(())
            })
            .unwrap();
            let (publication, prepared) = prepare(&store, &next, b"after", &control);
            assert!(publication.transaction().allocation() > aborted.allocation());
            let receipt = graph(&store, &control, |graph| {
                let receipt = store
                    .commit(publication.transaction(), &prepared, &control)
                    .unwrap();
                graph.resolve_publication(publication, CommitStatus::Committed(receipt))?;
                Ok(receipt)
            })
            .unwrap();
            (next.id(), receipt)
        };
        assert_eq!(control.memory().used(), 0);
        {
            // A lost response can be retried even after later transactions have committed.
            let store =
                crate::mvcc::restore::open(Database::open(&backup).unwrap(), request, &control)
                    .unwrap();
            assert_eq!(
                store
                    .commit_status(new_receipt.transaction, &control)
                    .unwrap(),
                CommitStatus::Committed(new_receipt)
            );
            assert!(store
                .snapshot(&control)
                .unwrap()
                .get(b"after", &control)
                .unwrap()
                .is_some());
            let next = actor(&store, &control);
            assert_eq!(next.id().coordinator(), new_actor.coordinator());
            assert!(next.id().allocation() > new_actor.allocation());
        }
        let original = RedbRecordStore::new(Arc::new(Database::open(&path).unwrap())).unwrap();
        assert_eq!(original.identity, request.source());
        assert_original_outcomes(&original, receipt, pending, aborted, &control);
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn failed_restore_admission_keeps_the_original_receipts_and_checkpoint() {
    let store = memory();
    let control = StorageReadControl::with_limit(1 << 20);
    let reader = actor(&store, &control);
    let publisher = actor(&store, &control);
    let (publication, prepared) = prepare(&store, &publisher, b"committed", &control);
    let receipt = graph(&store, &control, |graph| {
        let receipt = store
            .commit(publication.transaction(), &prepared, &control)
            .unwrap();
        graph.resolve_publication(publication, CommitStatus::Committed(receipt))?;
        Ok(receipt)
    })
    .unwrap();
    let old = reader.id();
    drop((reader, publisher, prepared));
    // Dead participant slots remain charged to this existing owner until reclamation or owner release.
    let retained = control.memory().used();
    let request = DatabaseRestore::new(store.identity).unwrap();
    let limited = StorageReadControl::with_limit(0);
    assert!(matches!(
        crate::mvcc::restore::publish(&store, request, &limited),
        Err(VersionError::Memory(_))
    ));
    assert_eq!(limited.memory().used(), 0);
    assert_eq!(control.memory().used(), retained);
    let cancelled = StorageReadControl::with_limit(1 << 20);
    cancelled.cancellation().cancel();
    let error = crate::mvcc::restore::publish(&store, request, &cancelled).unwrap_err();
    assert!(matches!(&error, VersionError::Cancelled(_)), "{error:?}");
    assert_eq!(
        store.commit_status(receipt.transaction, &control).unwrap(),
        CommitStatus::Committed(receipt)
    );
    graph(&store, &control, |graph| graph.check_active(old)).unwrap();

    // Restore is not a way to suppress an inconsistent terminal checkpoint.
    let transaction = physical_writer(&store.database).unwrap();
    transaction
        .open_table(TRANSACTIONS)
        .unwrap()
        .remove(receipt.transaction.allocation())
        .unwrap();
    transaction.commit().unwrap();
    assert!(matches!(
        crate::mvcc::restore::publish(&store, request, &control),
        Err(VersionError::UnknownTransaction)
    ));
    let transaction = store.database.begin_read().unwrap();
    assert_eq!(
        codec::database_id(&transaction.open_table(METADATA).unwrap()).unwrap(),
        request.source()
    );
    assert_eq!(control.memory().used(), retained);
    drop((transaction, store));
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn restore_synchronization_failure_leaves_one_complete_history_and_retry_converges() {
    let backend = FaultBackend::default();
    let control = StorageReadControl::with_limit(1 << 20);
    let (request, receipt, pending, old_actor) = {
        let database = Database::builder()
            .create_with_backend(backend.clone())
            .unwrap();
        let store = RedbRecordStore::new(Arc::new(database)).unwrap();
        let reader = actor(&store, &control);
        let publisher = actor(&store, &control);
        let (publication, prepared) = prepare(&store, &publisher, b"committed", &control);
        let receipt = graph(&store, &control, |graph| {
            let receipt = store
                .commit(publication.transaction(), &prepared, &control)
                .unwrap();
            graph.resolve_publication(publication, CommitStatus::Committed(receipt))?;
            Ok(receipt)
        })
        .unwrap();
        let pending = store.allocate_transaction(&control).unwrap();
        let old_actor = reader.id();
        drop((reader, publisher));
        let request = DatabaseRestore::new(store.identity).unwrap();
        backend.fail_sync.store(true, Ordering::Relaxed);
        assert!(crate::mvcc::restore::publish(&store, request, &control).is_err());
        assert!(!backend.fail_sync.load(Ordering::Relaxed));
        (request, receipt, pending, old_actor)
    };
    assert_eq!(control.memory().used(), 0);
    {
        let database = Database::builder()
            .create_with_backend(backend.clone())
            .unwrap();
        let store = RedbRecordStore::new(Arc::new(database)).unwrap();
        let needs_restore = request.needs_restore(store.identity).unwrap();
        let transaction = store.database.begin_read().unwrap();
        let receipts = transaction.open_table(TRANSACTIONS).unwrap();
        let metadata = transaction.open_table(METADATA).unwrap();
        if needs_restore {
            assert_eq!(
                status(&receipts, receipt.transaction).unwrap(),
                CommitStatus::Committed(receipt)
            );
            assert_eq!(status(&receipts, pending).unwrap(), CommitStatus::Pending);
            assert!(metadata.get("serializable").unwrap().is_some());
        } else {
            assert!(receipts.is_empty().unwrap());
            assert!(metadata.get("serializable").unwrap().is_none());
        }
        drop((receipts, metadata, transaction));
        assert_eq!(
            store
                .snapshot(&control)
                .unwrap()
                .get(b"committed", &control)
                .unwrap()
                .unwrap()
                .value()
                .map(|v| &***v),
            Some(b"durable".as_slice())
        );
    }
    let database = Database::builder().create_with_backend(backend).unwrap();
    let store = crate::mvcc::restore::open(database, request, &control).unwrap();
    assert_eq!(store.identity, request.target());
    assert!(matches!(
        store.commit_status(receipt.transaction, &control),
        Err(VersionError::WrongDatabase)
    ));
    let next = actor(&store, &control);
    assert_ne!(next.id().coordinator(), old_actor.coordinator());
}
