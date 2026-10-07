//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Evaluated batches transfer their retained bytes into private transaction history.

use super::*;

#[test]
fn evaluated_batch_payloads_fit_one_retained_copy_through_savepoint_undo() {
    let persistence = Persistence::new();
    let store = persistence.session(96 * 1024);
    let control = store.retention_control();
    let payload = vec![7; 64 * 1024];
    store.begin_transaction().unwrap();
    store.savepoint("before").unwrap();
    store
        .with_mutation(&mut |_, batch| batch.put(b"payload", &payload))
        .unwrap();
    let retained = store.record_snapshot().unwrap();
    store.rollback_to_savepoint("before").unwrap();
    assert_eq!(store.get(b"payload").unwrap(), None);
    retained
        .visit_value(b"payload", &control, &mut |record| {
            assert_eq!(record.unwrap().value, Some(payload.as_slice()));
            Ok(())
        })
        .unwrap();
    store.rollback_transaction().unwrap();
    assert!(control.memory().used() >= payload.len());
    drop(retained);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn acknowledgement_failure_keeps_terminal_completion_and_retries_without_republishing() {
    for implicit in [false, true] {
        let persistence = Persistence::new();
        let store = persistence.session(96 * 1024);
        persistence.state.lock().acknowledgement_fault = true;
        let error = if implicit {
            store.put(b"acknowledged", b"once").unwrap_err()
        } else {
            store.begin_transaction().unwrap();
            store.put(b"acknowledged", b"once").unwrap();
            store.commit_transaction().unwrap_err()
        };
        assert!(matches!(
            error.transaction_outcome(),
            Some(TransactionOutcome::Committed(_))
        ));
        let id = store.pending_commit().unwrap();
        assert!(store.completed_commit().is_none());
        let attempts = persistence.state.lock().attempts.len();
        let acknowledgement = persistence.state.lock().acknowledgements[0];
        assert_eq!(acknowledgement.transaction(), id);
        store.commit_transaction().unwrap();
        let state = persistence.state.lock();
        assert_eq!(state.attempts.len(), attempts);
        assert_eq!(state.acknowledgements, [acknowledgement, acknowledgement]);
        drop(state);
        assert!(!store.in_transaction());
        assert_eq!(store.completed_commit().unwrap().transaction, id);
        assert_eq!(
            store.get(b"acknowledged").unwrap().as_deref(),
            Some(b"once".as_slice())
        );
    }
}

#[test]
fn abort_acknowledgement_failure_preserves_the_confirmed_abort_for_retry() {
    let persistence = Persistence::new();
    let store = persistence.session(96 * 1024);
    store.begin_transaction().unwrap();
    store.put(b"aborted", b"private").unwrap();
    persistence.state.lock().commit_fault = CommitFault::LoseBeforeCommit;
    assert!(store.commit_transaction().is_err());
    let id = store.pending_commit().unwrap();
    persistence.state.lock().acknowledgement_fault = true;
    let error = store.rollback_transaction().unwrap_err();
    assert!(matches!(
        error.transaction_outcome(),
        Some(TransactionOutcome::Aborted(_))
    ));
    assert_eq!(store.pending_commit(), Some(id));
    store.rollback_transaction().unwrap();
    assert_eq!(
        persistence.state.lock().acknowledgements,
        [
            ReceiptAcknowledgement::Aborted(id),
            ReceiptAcknowledgement::Aborted(id)
        ]
    );
    assert_eq!(store.get(b"aborted").unwrap(), None);
    assert!(store.completed_commit().is_none());
}

#[test]
fn completed_receipts_are_session_local_and_need_no_snapshot_capture() {
    let persistence = Persistence::new();
    let store = persistence.session(96 * 1024);
    assert!(store.completed_commit().is_none());
    store.put(b"first", b"committed").unwrap();
    let captures = persistence.state.lock().captures;
    let receipt = store.completed_commit().unwrap();
    assert_eq!(persistence.state.lock().captures, captures);
    let peer = persistence.session(96 * 1024);
    assert!(peer.completed_commit().is_none());
    peer.put(b"peer", b"later").unwrap();
    assert_eq!(store.completed_commit(), Some(receipt));
    assert!(peer.completed_commit().unwrap().sequence > receipt.sequence);

    store.begin_read_transaction().unwrap();
    assert!(store.completed_commit().is_none());
    store.commit_transaction().unwrap();
    assert!(store.completed_commit().is_none());
    store.begin_transaction().unwrap();
    store.put(b"undone", b"private").unwrap();
    assert!(store.completed_commit().is_none());
    store.rollback_transaction().unwrap();
    assert!(store.completed_commit().is_none());
    store.begin_transaction().unwrap();
    store.put(b"explicit", b"committed").unwrap();
    store.commit_transaction().unwrap();
    assert!(store.completed_commit().unwrap().sequence > receipt.sequence);
}

#[test]
fn a_write_to_an_unused_key_expects_no_record_without_reading_one() {
    let persistence = Persistence::new();
    let store = persistence.session(1 << 20);
    let conflict = |result: uqa_storage::StorageBackendResult<()>| {
        let StorageBackendError::Backend { source, .. } = result.unwrap_err() else {
            panic!("typed MVCC conflict required")
        };
        assert!(matches!(
            source.downcast_ref::<VersionError>(),
            Some(VersionError::WriteConflict { .. })
        ));
    };
    // A key that never had a record takes the write.
    store
        .with_mutation(&mut |_, batch| batch.put_unused(b"new", b"first"))
        .unwrap();
    assert_eq!(store.get(b"new").unwrap().unwrap(), b"first");

    // A key with a record refuses it at commit and keeps its record.
    store.put(b"taken", b"kept").unwrap();
    store.begin_transaction().unwrap();
    store
        .with_mutation(&mut |_, batch| batch.put_unused(b"taken", b"replaced"))
        .unwrap();
    conflict(store.commit_transaction());
    store.rollback_transaction().unwrap();
    assert_eq!(store.get(b"taken").unwrap().unwrap(), b"kept");

    // So does a key whose record was deleted: its tombstone still has a revision, which is why a key that is merely absent does not qualify.
    store.delete(b"taken").unwrap();
    store.begin_transaction().unwrap();
    store
        .with_mutation(&mut |_, batch| batch.put_unused(b"taken", b"again"))
        .unwrap();
    conflict(store.commit_transaction());
    store.rollback_transaction().unwrap();
    assert!(store.get(b"taken").unwrap().is_none());

    // A key the transaction itself changed takes its condition from that change.
    store.begin_transaction().unwrap();
    store.put(b"own", b"first").unwrap();
    store
        .with_mutation(&mut |_, batch| batch.put_unused(b"own", b"second"))
        .unwrap();
    store.delete(b"new").unwrap();
    store
        .with_mutation(&mut |_, batch| batch.put_unused(b"new", b"restored"))
        .unwrap();
    store.commit_transaction().unwrap();
    assert_eq!(store.get(b"own").unwrap().unwrap(), b"second");
    assert_eq!(store.get(b"new").unwrap().unwrap(), b"restored");
}
