//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Execution cancellation follows publication while reads and rollback remain available.

use super::*;

#[test]
fn cancelled_publication_preserves_private_reads_and_allows_receipt_cleanup() {
    let persistence = Persistence::new();
    let session = persistence.session(1 << 20);
    session.put(b"committed", b"kept").unwrap();
    session.begin_transaction().unwrap();
    session.put(b"private", b"discarded").unwrap();
    persistence.state.lock().commit_fault = CommitFault::Reject;
    assert!(session.commit_transaction().is_err());
    let transaction = session.pending_commit().unwrap();
    persistence.state.lock().commit_fault = CommitFault::None;

    let cancel = session.write_cancellation().unwrap();
    cancel.cancel();
    assert!(matches!(
        session.commit_transaction(),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert_eq!(
        session.get(b"private").unwrap().as_deref(),
        Some(&b"discarded"[..])
    );
    session.rollback_transaction().unwrap();
    assert!(!session.in_transaction());
    assert_eq!(
        persistence
            .commit_status(transaction, &session.retention_control())
            .unwrap(),
        CommitStatus::Aborted
    );
    assert!(session.get(b"private").unwrap().is_none());
    assert_eq!(
        session.get(b"committed").unwrap().as_deref(),
        Some(&b"kept"[..])
    );
    cancel.reset();
    session.put(b"next", b"usable").unwrap();
}

#[test]
fn autonomous_sessions_share_write_cancellation_without_sharing_private_state_or_budget() {
    let persistence = Persistence::new();
    let root = persistence.session(1 << 20);
    root.begin_transaction().unwrap();
    root.put(b"private", b"root only").unwrap();
    let cancel = root.write_cancellation().unwrap();
    let child = root.open_session_with_cancellation(&cancel).unwrap();
    let sibling = root.open_session().unwrap();
    assert_ne!(child.transaction_affinity(), root.transaction_affinity());
    assert!(child.get(b"private").unwrap().is_none());
    let retained = root.retention_control().memory().used();
    child.begin_transaction().unwrap();
    child.put(b"child", b"not published").unwrap();
    assert_eq!(root.retention_control().memory().used(), retained);
    cancel.cancel();
    assert!(child.write_cancellation().unwrap().is_cancelled());
    assert!(!sibling.write_cancellation().unwrap().is_cancelled());
    assert!(matches!(
        child.commit_transaction(),
        Err(StorageBackendError::Cancelled(_))
    ));
    child.rollback_transaction().unwrap();
    root.rollback_transaction().unwrap();
    sibling.put(b"sibling", b"independent").unwrap();
    assert!(sibling.get(b"child").unwrap().is_none());
}

#[test]
fn cancelled_identifier_observations_restore_the_batch_and_leave_its_watermark_unchanged() {
    let persistence = Persistence::new();
    let session = persistence.session(1 << 20);
    session.begin_transaction().unwrap();
    session.put(b"private", b"kept").unwrap();
    let mut batch = session.batch();
    batch.put(b"private", b"overwritten").unwrap();
    batch.observe_identifier(b"identities", 41).unwrap();
    session.write_cancellation().unwrap().cancel();
    assert!(matches!(
        batch.commit(),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert_eq!(
        session.get(b"private").unwrap().as_deref(),
        Some(&b"kept"[..])
    );
    assert_eq!(session.identifier_watermark(b"identities").unwrap(), None);
    session.rollback_transaction().unwrap();
}
