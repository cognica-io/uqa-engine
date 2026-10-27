//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::mvcc::{CommitErrorOutcome, CommitReceipt, CommitSequence, DatabaseId};

fn receipt() -> CommitReceipt {
    CommitReceipt {
        transaction: StorageTransactionId::new(DatabaseId::from_bytes([23; 16]), 7).unwrap(),
        sequence: CommitSequence::from_u64(9),
        fingerprint: [13; 32],
    }
}

fn reject(event: RecordEvent) -> Option<RecordFault> {
    (event.boundary == RecordBoundary::Before)
        .then(|| RecordFault::Reject(uqa_core::memory::MemoryError::SizeOverflow.into()))
}

fn lose_reply(event: RecordEvent) -> Option<RecordFault> {
    (event.boundary == RecordBoundary::After).then(|| {
        RecordFault::Reject(StorageBackendError::Other("lost physical reply".into()).into())
    })
}

#[test]
fn rejected_calls_do_not_execute_and_successful_calls_execute_once() {
    let expected = receipt();
    with_record_faults(reject, || {
        assert!(matches!(
            commit(expected.transaction, || panic!("rejected commit ran")),
            Err(CommitFailure::Rejected(VersionError::Memory(_)))
        ));
        assert!(matches!(
            abort(expected.transaction, || panic!("rejected abort ran")),
            Err(VersionError::Memory(_))
        ));
    });
    assert_eq!(
        commit(expected.transaction, || Ok(expected)).unwrap(),
        expected
    );
    assert_eq!(
        abort(expected.transaction, || Ok(CommitStatus::Aborted)).unwrap(),
        CommitStatus::Aborted
    );
}

#[test]
fn missing_replies_keep_identity_and_never_claim_rejection_after_execution() {
    let expected = receipt();
    let calls = Cell::new(0);
    with_record_faults(lose_reply, || {
        let error = commit(expected.transaction, || {
            calls.set(calls.get() + 1);
            Ok(expected)
        })
        .unwrap_err();
        assert!(
            matches!(error, CommitFailure::Indeterminate { transaction, .. } if transaction == expected.transaction)
        );
        let error = abort(expected.transaction, || {
            calls.set(calls.get() + 1);
            Ok(CommitStatus::Aborted)
        })
        .unwrap_err()
        .into_storage_error();
        assert_eq!(
            error.commit_outcome(),
            Some(CommitErrorOutcome::Indeterminate(expected.transaction))
        );
    });
    assert_eq!(calls.get(), 2);
}

#[test]
fn native_errors_precede_reply_faults() {
    let expected = receipt();
    with_record_faults(lose_reply, || {
        let error = commit(expected.transaction, || {
            Err(VersionError::IdentifiersExhausted.into())
        })
        .unwrap_err();
        assert!(matches!(
            error,
            CommitFailure::Rejected(VersionError::IdentifiersExhausted)
        ));
        assert!(matches!(
            abort(expected.transaction, || Err(
                VersionError::SequenceExhausted
            )),
            Err(VersionError::SequenceExhausted)
        ));
    });
}

#[test]
fn nested_scopes_restore_after_unwind_and_do_not_select_other_threads() {
    let expected = receipt();
    with_record_faults(reject, || {
        assert!(
            std::panic::catch_unwind(|| with_record_faults(lose_reply, || {
                panic!("native fault scope unwind");
            }))
            .is_err()
        );
        assert!(matches!(
            commit(expected.transaction, || Ok(expected)),
            Err(CommitFailure::Rejected(VersionError::Memory(_)))
        ));
        std::thread::spawn(move || {
            assert_eq!(
                commit(expected.transaction, || Ok(expected)).unwrap(),
                expected
            );
        })
        .join()
        .unwrap();
    });
    assert_eq!(
        commit(expected.transaction, || Ok(expected)).unwrap(),
        expected
    );
}
