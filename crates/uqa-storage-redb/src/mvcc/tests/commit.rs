//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical encoding reserves one reusable payload and publishes only complete batches.

use super::*;
use uqa_storage::mvcc::RecordWrite;

#[test]
fn payload_workspace_failure_keeps_pending_records_retryable() {
    let database = Arc::new(
        Database::builder()
            .create_with_backend(InMemoryBackend::new())
            .unwrap(),
    );
    let store = RedbRecordStore::new(database).unwrap();
    let control = StorageReadControl::with_limit(1 << 20);
    let payload = vec![7; 4096];
    let prepared = PreparedRecordCommit::new(
        &[
            RecordWrite {
                key: b"value",
                expected: None,
                value: Some(&payload),
            },
            RecordWrite {
                key: b"empty",
                expected: None,
                value: Some(b""),
            },
            RecordWrite {
                key: b"deleted",
                expected: None,
                value: None,
            },
        ],
        &control,
    )
    .unwrap();
    let id = store.allocate_transaction(&control).unwrap();
    let retained = control.memory().used();
    let hold = control
        .memory()
        .reserve(control.memory().limit() - retained - payload.len())
        .unwrap();
    assert!(matches!(
        store.commit(id, &prepared, &control),
        Err(CommitFailure::Rejected(VersionError::Memory(_)))
    ));
    drop(hold);
    assert_eq!(control.memory().used(), retained);
    assert_eq!(
        store.commit_status(id, &control).unwrap(),
        CommitStatus::Pending
    );
    {
        let snapshot = store.snapshot(&control).unwrap();
        assert_eq!(snapshot.sequence(), CommitSequence::from_u64(0));
        assert!(snapshot.get(b"value", &control).unwrap().is_none());
        assert!(snapshot.get(b"empty", &control).unwrap().is_none());
        assert!(snapshot.get(b"deleted", &control).unwrap().is_none());
    }
    let receipt = store.commit(id, &prepared, &control).unwrap();
    assert_eq!(receipt.fingerprint, prepared.fingerprint());
    assert_eq!(control.memory().used(), retained);
    let snapshot = store.snapshot(&control).unwrap();
    for (key, expected) in [
        (b"value".as_slice(), Some(payload.as_slice())),
        (b"empty", Some(b"".as_slice())),
        (b"deleted", None),
    ] {
        let record = snapshot.get(key, &control).unwrap().unwrap();
        assert_eq!(record.sequence(), receipt.sequence);
        assert_eq!(record.value().map(|value| &***value), expected);
    }
}
