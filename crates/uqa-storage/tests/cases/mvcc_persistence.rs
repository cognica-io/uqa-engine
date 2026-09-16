//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Prepared transaction identity and authoritative receipt interpretation.

use uqa_storage::mvcc::{
    resolve_prepared_receipt, CommitReceipt, CommitSequence, CommitStatus, DatabaseId,
    PreparedRecordCommit, RecordVersion, RecordWrite, SharedRecordValue, StorageTransactionId,
    VersionError,
};
use uqa_storage::read_control::StorageReadControl;

#[test]
fn prepared_fingerprints_distinguish_boundaries_preconditions_and_tombstones() {
    let control = StorageReadControl::with_limit(1 << 20);
    let fingerprint = |key: &[u8], expected, value| {
        PreparedRecordCommit::new(
            &[RecordWrite {
                key,
                expected,
                value,
            }],
            &control,
        )
        .unwrap()
        .fingerprint()
    };
    let original = fingerprint(b"key", None, Some(b"value"));
    assert_eq!(original, fingerprint(b"key", None, Some(b"value")));
    assert_ne!(original, fingerprint(b"ke", None, Some(b"yvalue")));
    assert_ne!(
        original,
        fingerprint(b"key", Some(CommitSequence::from_u64(1)), Some(b"value"))
    );
    assert_ne!(
        fingerprint(b"key", None, None),
        fingerprint(b"key", None, Some(b""))
    );
    assert_ne!(
        fingerprint(b"key", None, None),
        fingerprint(b"key", Some(CommitSequence::INITIAL), None)
    );
}

#[test]
fn receipt_resolution_never_interprets_missing_evidence_as_an_abort() {
    let id = StorageTransactionId::new(DatabaseId::from_bytes([1; 16]), 7).unwrap();
    let receipt = CommitReceipt {
        transaction: id,
        sequence: CommitSequence::from_u64(9),
        fingerprint: [42; 32],
    };
    assert_eq!(
        resolve_prepared_receipt(CommitStatus::Committed(receipt), id, [42; 32]).unwrap(),
        Some(receipt)
    );
    assert_eq!(
        resolve_prepared_receipt(CommitStatus::Pending, id, [42; 32]).unwrap(),
        None
    );
    assert!(matches!(
        resolve_prepared_receipt(CommitStatus::Unknown, id, [42; 32]),
        Err(VersionError::UnknownTransaction)
    ));
    assert!(matches!(
        resolve_prepared_receipt(CommitStatus::Aborted, id, [42; 32]),
        Err(VersionError::TransactionFinished)
    ));
    assert!(matches!(
        resolve_prepared_receipt(CommitStatus::Committed(receipt), id, [0; 32]),
        Err(VersionError::CommitMismatch)
    ));
    let wrong = StorageTransactionId::new(id.database(), 8).unwrap();
    assert!(matches!(
        resolve_prepared_receipt(CommitStatus::Committed(receipt), wrong, [42; 32]),
        Err(VersionError::CommitMismatch)
    ));
}

#[test]
fn invalid_decoded_identifiers_fail_before_payload_allocation() {
    let control = StorageReadControl::with_limit(0);
    assert!(matches!(
        StorageTransactionId::new(DatabaseId::from_bytes([1; 16]), 0),
        Err(VersionError::InvalidTransactionId)
    ));
    assert!(matches!(
        RecordVersion::<SharedRecordValue>::copy_bytes(
            CommitSequence::INITIAL,
            Some(&[0; 128]),
            &control
        ),
        Err(VersionError::InvalidEncoding(_))
    ));
    assert_eq!(control.memory().peak(), 0);
}
