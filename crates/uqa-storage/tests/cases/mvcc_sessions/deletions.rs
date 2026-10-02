//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn idempotent_prefix_deletion_accepts_cleanup_without_deleting_later_keys() {
    for refresh in [false, true] {
        let persistence = Persistence::new();
        let a = persistence.session(1 << 20);
        let b = persistence.session(1 << 20);
        a.put(b"journal/old", b"old").unwrap();
        a.put(b"journal/kept", b"kept until publication").unwrap();
        a.begin_transaction().unwrap();
        let retained = a.record_snapshot().unwrap();
        a.savepoint("before").unwrap();
        let mut batch = a.batch();
        batch.delete_prefix_allow_absent(b"journal/").unwrap();
        batch.commit().unwrap();
        b.delete(b"journal/old").unwrap();
        b.put(b"journal/later", b"not evaluated").unwrap();
        if refresh {
            a.refresh_transaction_snapshot(a.retention_control().cancellation())
                .unwrap();
            assert!(a.get(b"journal/old").unwrap().is_none());
            assert!(a.get(b"journal/kept").unwrap().is_none());
            assert_eq!(a.get(b"journal/later").unwrap().unwrap(), b"not evaluated");
            a.rollback_to_savepoint("before").unwrap();
            assert_eq!(a.get(b"journal/old").unwrap().unwrap(), b"old");
            assert!(a.get(b"journal/later").unwrap().is_none());
            let mut batch = a.batch();
            batch.delete_prefix_allow_absent(b"journal/").unwrap();
            batch.commit().unwrap();
            a.refresh_transaction_snapshot(a.retention_control().cancellation())
                .unwrap();
        }
        a.commit_transaction().unwrap();
        assert_eq!(
            b.scan_prefix(b"journal/").unwrap(),
            vec![(b"journal/later".to_vec(), b"not evaluated".to_vec())]
        );
        retained
            .visit_value(b"journal/old", &a.retention_control(), &mut |record| {
                assert_eq!(record.unwrap().value, Some(b"old".as_slice()));
                Ok(())
            })
            .unwrap();
    }
}

#[test]
fn idempotent_deletion_rebases_onto_cleanup_committed_after_a_lost_reply() {
    let persistence = Persistence::new();
    let a = persistence.session(1 << 20);
    let b = persistence.session(1 << 20);
    a.put(b"journal/old", b"old").unwrap();
    a.put(b"journal/kept", b"kept until publication").unwrap();
    a.begin_transaction().unwrap();
    let mut batch = a.batch();
    batch.delete_prefix_allow_absent(b"journal/").unwrap();
    batch.commit().unwrap();
    let start = persistence.state.lock().attempts.len();
    persistence.state.lock().commit_fault = CommitFault::LoseBeforeCommit;
    assert!(a.commit_transaction().is_err());
    let id = a.pending_commit().unwrap();
    // Cleanup removes a key the pending deletion covers and commits before the attempt resolves.
    b.begin_transaction().unwrap();
    b.delete(b"journal/old").unwrap();
    b.put(b"journal/later", b"not evaluated").unwrap();
    b.commit_transaction().unwrap();
    a.commit_transaction().unwrap();
    let state = persistence.state.lock();
    // The provider rejects the outdated derived snapshot of the original attempt once; storage rebases only the deletions and presents the same fingerprint again.
    let attempts = &state.attempts[start..];
    assert_eq!(attempts.len(), 4);
    assert_eq!(attempts[2], attempts[0]);
    assert_eq!(attempts[3], attempts[0]);
    let CommitStatus::Committed(receipt) = state.receipts[&id.allocation()] else {
        panic!("the original attempt commits")
    };
    assert_eq!(receipt.fingerprint, attempts[0]);
    drop(state);
    assert_eq!(
        b.scan_prefix(b"journal/").unwrap(),
        vec![(b"journal/later".to_vec(), b"not evaluated".to_vec())]
    );
}

#[test]
fn idempotent_prefix_deletion_preserves_live_replacement_and_explicit_fence_conflicts() {
    for refresh in [false, true] {
        for mode in 0..4 {
            let persistence = Persistence::new();
            let a = persistence.session(1 << 20);
            let b = persistence.session(1 << 20);
            a.put(b"journal/old", b"original").unwrap();
            a.begin_transaction().unwrap();
            let mut batch = a.batch();
            if mode == 1 {
                batch.require_unchanged(b"journal/old").unwrap();
            }
            if mode == 2 {
                batch.fence_record(b"journal/old").unwrap();
            }
            if mode == 3 {
                batch.delete_prefix(b"journal/").unwrap();
            } else {
                batch.delete_prefix_allow_absent(b"journal/").unwrap();
            }
            batch.put(b"unrelated", b"must remain private").unwrap();
            batch.commit().unwrap();
            if mode == 0 {
                b.put(b"journal/old", b"original").unwrap();
            } else {
                b.delete(b"journal/old").unwrap();
            }
            let result = if refresh {
                a.refresh_transaction_snapshot(a.retention_control().cancellation())
            } else {
                a.commit_transaction()
            };
            let StorageBackendError::Backend { source, .. } = result.unwrap_err() else {
                panic!("typed MVCC conflict required")
            };
            assert!(matches!(
                source.downcast_ref::<VersionError>(),
                Some(VersionError::ReadConflict { .. } | VersionError::WriteConflict { .. })
            ));
            a.rollback_transaction().unwrap();
            assert!(b.get(b"unrelated").unwrap().is_none());
        }
    }
}
