//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retained terminal owners release retry rights without another physical commit.

use super::*;
use uqa_storage::mvcc::RetainedTransactionAllocation;

#[test]
fn retained_completion_never_commits_and_waits_for_lease_release_before_reclamation() {
    for mode in 0..5 {
        for native in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let connection = open(&directory.path().join("completion.db"), mode);
            let control = control();
            let store = records(&connection, native, &control);
            let commits = record_commits(&store);
            for committed in [false, true] {
                let owner = store.allocate_managed_transaction(&control).unwrap();
                let id = owner.transaction();
                let acknowledgement = if committed {
                    ReceiptAcknowledgement::Committed(
                        store.commit(id, &empty(&control), &control).unwrap(),
                    )
                } else {
                    store.abort(id, &control).unwrap();
                    ReceiptAcknowledgement::Aborted(id)
                };
                let expected = store.commit_status(id, &control).unwrap();
                commits.lock().clear();
                for _ in 0..2 {
                    store
                        .acknowledge_retained_transaction(&owner, acknowledgement, &control)
                        .unwrap();
                }
                assert!(commits.lock().is_empty(), "mode {mode}, native {native}");
                assert_eq!(store.reclaim_transaction_receipts(&control).unwrap(), 0);
                assert_eq!(store.commit_status(id, &control).unwrap(), expected);
                drop(owner);
                assert_eq!(store.reclaim_transaction_receipts(&control).unwrap(), 1);
                assert_eq!(
                    store.commit_status(id, &control).unwrap(),
                    CommitStatus::Unknown
                );
                store
                    .acknowledge_transaction(acknowledgement, &control)
                    .unwrap();
            }
        }
    }
}

#[test]
fn retained_completion_rejects_pending_mismatched_and_cancelled_outcomes() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let store = SQLiteRecordStore::new(&connection).unwrap();
    let control = control();
    let owner = store.allocate_managed_transaction(&control).unwrap();
    let id = owner.transaction();
    assert!(matches!(
        store.acknowledge_retained_transaction(
            &owner,
            ReceiptAcknowledgement::Aborted(id),
            &control
        ),
        Err(VersionError::TransactionSealed)
    ));
    let receipt = store.commit(id, &empty(&control), &control).unwrap();
    let acknowledgement = ReceiptAcknowledgement::Committed(receipt);
    let commits = record_commits(&store);
    assert!(matches!(
        store.acknowledge_retained_transaction(&owner, ReceiptAcknowledgement::Aborted(id), &control),
        Err(VersionError::AlreadyCommitted(actual)) if actual == receipt
    ));
    assert!(matches!(
        store.acknowledge_retained_transaction(
            &owner,
            ReceiptAcknowledgement::Committed(uqa_storage::mvcc::CommitReceipt {
                fingerprint: [42; 32],
                ..receipt
            }),
            &control,
        ),
        Err(VersionError::AlreadyCommitted(actual)) if actual == receipt
    ));
    let foreign = SQLiteRecordStore::new(&ManagedConnection::open_in_memory().unwrap()).unwrap();
    assert!(matches!(
        foreign.acknowledge_retained_transaction(&owner, acknowledgement, &control),
        Err(VersionError::WrongDatabase)
    ));
    let cancelled = StorageReadControl::with_limit(1 << 20);
    cancelled.cancellation().cancel();
    assert!(store
        .acknowledge_retained_transaction(&owner, acknowledgement, &cancelled)
        .is_err());
    assert!(commits.lock().is_empty());
    assert_eq!(store.reclaim_transaction_receipts(&control).unwrap(), 0);
    assert_eq!(
        store.commit_status(id, &control).unwrap(),
        CommitStatus::Committed(receipt)
    );
    let other = store.allocate_managed_transaction(&control).unwrap();
    assert!(matches!(
        store.acknowledge_retained_transaction(&other, acknowledgement, &control),
        Err(VersionError::InvalidEncoding(
            "receipt acknowledgement owner mismatch"
        ))
    ));
    store
        .acknowledge_retained_transaction(&owner, acknowledgement, &control)
        .unwrap();
    drop(owner);
    assert_eq!(store.reclaim_transaction_receipts(&control).unwrap(), 1);
}

#[test]
fn untracked_and_manual_owners_keep_explicit_durable_acknowledgement() {
    let directory = tempfile::tempdir().unwrap();
    let connection = ManagedConnection::open(&directory.path().join("manual.db")).unwrap();
    let store = SQLiteRecordStore::new(&connection).unwrap();
    let control = control();
    let id = store.allocate_transaction(&control).unwrap();
    let receipt = store.commit(id, &empty(&control), &control).unwrap();
    let commits = record_commits(&store);
    store
        .acknowledge_retained_transaction(
            &RetainedTransactionAllocation::untracked(id),
            ReceiptAcknowledgement::Committed(receipt),
            &control,
        )
        .unwrap();
    assert_eq!(*commits.lock(), [false]);
    assert_eq!(store.reclaim_transaction_receipts(&control).unwrap(), 1);

    // A matching handle alone does not turn a manually allocated row into managed ownership.
    let manual = store.allocate_transaction(&control).unwrap();
    store.abort(manual, &control).unwrap();
    let owner = store
        .with_receipt_admission(&control, |leases| {
            RetainedTransactionAllocation::retain(
                manual,
                leases.retain(uqa_storage::mvcc::receipt_lease_id(manual), &control)?,
            )
        })
        .unwrap();
    commits.lock().clear();
    store
        .acknowledge_retained_transaction(&owner, ReceiptAcknowledgement::Aborted(manual), &control)
        .unwrap();
    assert_eq!(*commits.lock(), [false]);
    assert_eq!(store.reclaim_transaction_receipts(&control).unwrap(), 1);
}

#[test]
fn retained_completion_preserves_overlapping_serializable_references() {
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
        .acknowledge_retained_transaction(
            &owner,
            ReceiptAcknowledgement::Committed(receipt),
            &control,
        )
        .unwrap();
    drop((owner, writer));
    assert_eq!(store.reclaim_transaction_receipts(&control).unwrap(), 0);
    assert_eq!(
        store.commit_status(id, &control).unwrap(),
        CommitStatus::Committed(receipt)
    );
    store
        .with_serializable_admission(&control, &mut |graph, _| {
            assert!(graph.retains_transaction_receipt(id));
            graph.rollback(older.id())
        })
        .unwrap();
    drop(older);
    assert_eq!(store.reclaim_transaction_receipts(&control).unwrap(), 1);
}

#[test]
fn autonomous_sequence_logs_need_only_publication_and_amortized_allocation_commits() {
    use uqa_storage::mvcc::{VersionedKeyValueStore, VersionedSessionOptions};
    use uqa_storage::{CatalogFacade, KeyValueCatalog, SequenceLogResult, SequenceValuePosition};

    let directory = tempfile::tempdir().unwrap();
    let connection = ManagedConnection::open(&directory.path().join("sequences.db")).unwrap();
    let store = Arc::new(SQLiteRecordStore::new(&connection).unwrap());
    let session =
        VersionedKeyValueStore::new(store.clone(), None, VersionedSessionOptions::default());
    let catalog = KeyValueCatalog::new(Arc::new(session.new_session()));
    catalog.save_schema("public").unwrap();
    let row = uqa_storage::SequenceRow {
        relation: uqa_storage::RelationIdentity::new("public", "sequence"),
        security: uqa_storage::SequenceSecurityRow::Bound(
            uqa_core::catalog_sequence::BoundSequenceSecurity::owner(
                uqa_core::catalog_role::RoleIdentity {
                    oid: 20_001,
                    object_id: [9; 16],
                },
            ),
        ),
        object_id: [1; 16],
        definition_generation: [2; 16],
        start: 1,
        increment: 1,
        current: 1,
        called: false,
        log_count: 0,
        persistence: "p".into(),
        owner: None,
        options: uqa_storage::SequenceOptions {
            min_value: Some(1),
            max_value: Some(i64::MAX),
            cache_size: 1,
            ..uqa_storage::SequenceOptions::default()
        },
    };
    catalog.create_sequence_row(&row).unwrap();
    let commits = record_commits(&store);
    for log in 1..=47 {
        if log == 32 {
            commits.lock().clear();
        }
        let independent = KeyValueCatalog::new(Arc::new(session.new_session()));
        let expected = if log == 1 {
            (1, false)
        } else {
            ((log - 1) * 33, true)
        };
        assert_eq!(
            independent
                .log_sequence_values(
                    "public.sequence",
                    row.object_id,
                    row.definition_generation,
                    expected,
                    SequenceValuePosition {
                        current: log * 33,
                        called: true,
                        log_count: 0
                    },
                )
                .unwrap(),
            SequenceLogResult::Logged
        );
    }
    // Sixteen independent sequence logs need sixteen durable publications and one
    // amortized allocation transaction; completion contributes no extra commit.
    assert_eq!(*commits.lock(), vec![false; 17]);
    let stored = catalog.load_sequence_rows().unwrap();
    assert_eq!(
        (stored[0].current, stored[0].called, stored[0].log_count),
        (47 * 33, true, 0)
    );
    drop((catalog, session, store, connection));
    let connection = ManagedConnection::open(&directory.path().join("sequences.db")).unwrap();
    let store = Arc::new(SQLiteRecordStore::new(&connection).unwrap());
    let catalog = KeyValueCatalog::new(Arc::new(VersionedKeyValueStore::new(
        store,
        None,
        VersionedSessionOptions::default(),
    )));
    assert_eq!(catalog.load_sequence_rows().unwrap(), stored);
}
