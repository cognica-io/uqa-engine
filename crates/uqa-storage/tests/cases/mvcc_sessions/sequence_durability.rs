//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Completion retains sequence obligations and exact publication outcomes after failure.

use super::*;

#[test]
fn sequence_publishers_cannot_outlive_or_change_their_original_completion_owner() {
    let persistence = Persistence::new();
    let store = persistence.session(96 * 1024);
    let cancellation = uqa_core::CancellationToken::new();
    store.begin_transaction().unwrap();
    let reader = store.new_retained_read_session(&cancellation).unwrap();
    let child = reader.new_sequence_value_session(&cancellation);
    let first = persistence.state.lock().sequence_publications[0].clone();
    assert!(first.with_publication(|deferred| deferred));
    drop(child);
    assert!(first.with_publication(|deferred| deferred));
    store.commit_transaction().unwrap();
    assert!(!first.with_publication(|deferred| deferred));
    store.begin_transaction().unwrap();
    assert!(!first.with_publication(|deferred| deferred));
    let nested = reader.new_retained_read_session(&cancellation).unwrap();
    assert!(nested.require_sequence_value_durability().is_err());
    drop(nested.new_sequence_value_session(&cancellation));
    assert_eq!(persistence.state.lock().sequence_publications.len(), 1);
    let child = store.new_sequence_value_session(&cancellation);
    let second = persistence.state.lock().sequence_publications[1].clone();
    assert!(second.with_publication(|deferred| deferred));
    drop(store);
    assert!(!second.with_publication(|deferred| deferred));
    assert!(reader.require_sequence_value_durability().is_err());
    drop(child);
}

#[test]
fn sequence_barrier_failure_retains_read_only_or_committed_completion_without_replay() {
    for written in [false, true] {
        let persistence = Persistence::new();
        let store = persistence.session(96 * 1024);
        store.begin_transaction().unwrap();
        store.savepoint("before").unwrap();
        let reader = store
            .new_retained_read_session(&uqa_core::CancellationToken::new())
            .unwrap();
        reader.require_sequence_value_durability().unwrap();
        store.rollback_to_savepoint("before").unwrap();
        if written {
            store.put(b"written", b"once").unwrap();
        }
        persistence.state.lock().sequence_sync_fault = true;
        let error = store.commit_transaction().unwrap_err();
        if written {
            assert!(matches!(
                error.transaction_outcome(),
                Some(TransactionOutcome::Committed(_))
            ));
        } else {
            assert!(error.transaction_outcome().is_none());
        }
        assert!(store.in_transaction());
        let attempts = persistence.state.lock().attempts.len();
        assert_eq!(attempts, usize::from(written));
        store.commit_transaction().unwrap();
        assert!(!store.in_transaction());
        assert_eq!(persistence.state.lock().attempts.len(), attempts);
        assert_eq!(persistence.state.lock().sequence_syncs, 2);
        store.begin_transaction().unwrap();
        store.commit_transaction().unwrap();
        assert_eq!(persistence.state.lock().sequence_syncs, 2);
    }
}

#[test]
fn sequence_rollback_retries_cleanup_after_cancellation_without_replaying_values() {
    let persistence = Persistence::new();
    let cancel = uqa_core::CancellationToken::new();
    let store = persistence
        .session(96 * 1024)
        .new_session_with_cancellation(&cancel);
    store.begin_transaction().unwrap();
    store.require_sequence_value_durability().unwrap();
    store.put(b"discarded", b"private").unwrap();
    cancel.cancel();
    persistence.state.lock().sequence_sync_fault = true;
    assert!(store.rollback_transaction().is_err());
    assert!(store.in_transaction());
    store.rollback_transaction().unwrap();
    assert!(!store.in_transaction());
    assert_eq!(persistence.state.lock().sequence_syncs, 2);
    assert_eq!(store.get(b"discarded").unwrap(), None);
    assert!(persistence.state.lock().attempts.is_empty());
}
