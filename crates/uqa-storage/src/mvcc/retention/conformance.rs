//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical providers share history, tombstone and retained-receipt acceptance schedules.

use crate::mvcc::{
    CommitFailure, CommitStatus, PreparedRecordCommit, RecordWrite, VersionError, VersionResult,
    VersionedPersistence,
};
use crate::read_control::StorageReadControl;

/// Verify reclamation on fresh disposable byte-record persistence. Keep snapshots through collection, then release them; an old successful receipt must still resolve after its record version disappears.
pub fn verify_version_reclamation(store: &dyn VersionedPersistence) -> VersionResult<()> {
    let control = StorageReadControl::with_limit(1 << 20);
    let empty = store.snapshot(&control)?;
    let first = PreparedRecordCommit::new(
        &[RecordWrite {
            key: b"retention-a",
            expected: None,
            value: Some(b"first"),
        }],
        &control,
    )?;
    let first_id = store.allocate_transaction(&control)?;
    let first_receipt = store.commit(first_id, &first, &control).map_err(rejected)?;
    let old = store.snapshot(&control)?;
    let clone = old.clone();
    let second = PreparedRecordCommit::new(
        &[
            RecordWrite {
                key: b"retention-a",
                expected: Some(first_receipt.sequence),
                value: Some(b"second"),
            },
            RecordWrite {
                key: b"retention-b",
                expected: None,
                value: Some(b"later"),
            },
        ],
        &control,
    )?;
    let second_id = store.allocate_transaction(&control)?;
    let second_receipt = store
        .commit(second_id, &second, &control)
        .map_err(rejected)?;
    let removed = PreparedRecordCommit::new(
        &[RecordWrite {
            key: b"retention-a",
            expected: Some(second_receipt.sequence),
            value: None,
        }],
        &control,
    )?;
    let removed_id = store.allocate_transaction(&control)?;
    let removed_receipt = store
        .commit(removed_id, &removed, &control)
        .map_err(rejected)?;
    let deleted = store.snapshot(&control)?;
    assert_eq!(store.reclaim_versions(&control)?, 0);
    assert!(empty.get(b"retention-a", &control)?.is_none());
    assert!(old.get(b"retention-b", &control)?.is_none());
    drop(empty);
    drop(old);
    assert_eq!(store.reclaim_versions(&control)?, 0);
    assert_eq!(
        &***clone
            .get(b"retention-a", &control)?
            .unwrap()
            .value()
            .unwrap(),
        b"first"
    );
    drop(clone);
    assert_eq!(store.reclaim_versions(&control)?, 2);
    let tombstone = deleted.get(b"retention-a", &control)?.unwrap();
    assert!(tombstone.value().is_none());
    assert_eq!(tombstone.sequence(), removed_receipt.sequence);
    assert_eq!(
        &***deleted
            .get(b"retention-b", &control)?
            .unwrap()
            .value()
            .unwrap(),
        b"later"
    );
    assert_eq!(
        store.commit(first_id, &first, &control).map_err(rejected)?,
        first_receipt
    );
    assert_eq!(
        store.commit_status(second_id, &control)?,
        CommitStatus::Committed(second_receipt)
    );
    assert_eq!(
        store.abort(removed_id, &control)?,
        CommitStatus::Committed(removed_receipt)
    );
    drop(deleted);
    assert_eq!(store.reclaim_versions(&control)?, 0);
    let stale_id = store.allocate_transaction(&control)?;
    assert!(matches!(
        store.commit(stale_id, &first, &control),
        Err(CommitFailure::Rejected(VersionError::WriteConflict { .. }))
    ));
    assert_eq!(store.abort(stale_id, &control)?, CommitStatus::Aborted);
    verify_head_and_cancellation(store, removed_receipt.sequence, &control)
}

fn verify_head_and_cancellation(
    store: &dyn VersionedPersistence,
    sequence: crate::mvcc::CommitSequence,
    control: &StorageReadControl,
) -> VersionResult<()> {
    let latest = store.snapshot(control)?;
    assert_eq!(latest.sequence(), sequence);
    assert!(latest
        .get(b"retention-a", control)?
        .unwrap()
        .value()
        .is_none());
    control.cancellation().cancel();
    assert!(matches!(
        store.reclaim_versions(control),
        Err(VersionError::Cancelled(_))
    ));
    Ok(())
}

fn rejected(error: CommitFailure) -> VersionError {
    match error {
        CommitFailure::Rejected(error) => error,
        error @ CommitFailure::Indeterminate { .. } => {
            VersionError::Storage(crate::StorageBackendError::Other(error.to_string()))
        }
    }
}
