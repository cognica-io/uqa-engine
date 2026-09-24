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
        let attempts = persistence.state.lock().attempts.len();
        let acknowledgement = persistence.state.lock().acknowledgements[0];
        assert_eq!(acknowledgement.transaction(), id);
        store.commit_transaction().unwrap();
        let state = persistence.state.lock();
        assert_eq!(state.attempts.len(), attempts);
        assert_eq!(state.acknowledgements, [acknowledgement, acknowledgement]);
        drop(state);
        assert!(!store.in_transaction());
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
}
