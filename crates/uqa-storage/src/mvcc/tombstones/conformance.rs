//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Provider schedules distinguish an old absence observation from a fresh one after physical retirement.

use crate::mvcc::{
    CommitFailure, CommitReceipt, CommitStatus, PreparedRecordCommit, RecordWrite,
    TombstoneReclamationRequest, TombstoneReclamationStep, VersionError, VersionResult,
    VersionedPersistence,
};
use crate::read_control::StorageReadControl;

/// Verify on fresh disposable persistence. Histories, deleted-snapshot revisions and original receipts survive until their own retention boundaries; only enrolled physical tombstones disappear.
pub fn verify_tombstone_reclamation(store: &dyn VersionedPersistence) -> VersionResult<()> {
    let control = StorageReadControl::with_limit(1 << 20);
    verify_reserved_identifiers(store, &control)?;
    let empty = store.snapshot(&control)?;
    assert_eq!(empty.reclamation_epoch(), Some(0));
    let writes = [RecordWrite {
        key: b"retired/key",
        expected: None,
        value: Some(b"first"),
    }];
    let stale = PreparedRecordCommit::new_at_snapshot(&writes, &*empty, &control)?;
    let first = PreparedRecordCommit::new(&writes, &control)?;
    drop(empty);
    let id = store.allocate_transaction(&control)?;
    let receipt = store.commit(id, &first, &control).map_err(rejected)?;
    let outside = publish(
        store,
        &[RecordWrite {
            key: b"unscoped/key",
            expected: None,
            value: Some(b"outside"),
        }],
        &control,
    )?;
    let deleted = publish(
        store,
        &[
            RecordWrite {
                key: b"retired/key",
                expected: Some(receipt.sequence),
                value: None,
            },
            RecordWrite {
                key: b"unscoped/key",
                expected: Some(outside.sequence),
                value: None,
            },
        ],
        &control,
    )?;
    let request = TombstoneReclamationRequest {
        prefix: b"retired/",
        after: None,
        through: deleted.sequence,
    };
    assert!(
        matches!(
            store.reclaim_tombstones(&request, &control)?,
            TombstoneReclamationStep::Complete { removed: 0 }
        ),
        "unpruned histories must wait for ordinary history GC"
    );
    let held = store.snapshot(&control)?;
    let clone = held.clone();
    store.reclaim_versions(&control)?;
    assert!(matches!(
        store.reclaim_tombstones(&request, &control)?,
        TombstoneReclamationStep::Retained
    ));
    drop(held);
    assert!(matches!(
        store.reclaim_tombstones(&request, &control)?,
        TombstoneReclamationStep::Retained
    ));
    assert_eq!(
        clone.get(b"retired/key", &control)?.unwrap().sequence(),
        deleted.sequence
    );
    drop(clone);
    assert!(store
        .reclaim_tombstones(&request, &StorageReadControl::with_limit(0))
        .is_err());
    let cancelled = StorageReadControl::with_limit(1 << 20);
    cancelled.cancellation().cancel();
    assert!(store.reclaim_tombstones(&request, &cancelled).is_err());
    assert_eq!(store.snapshot(&control)?.reclamation_epoch(), Some(0));
    assert!(matches!(
        store.reclaim_tombstones(&request, &control)?,
        TombstoneReclamationStep::Complete { removed: 1 }
    ));
    verify_reserved_identifiers(store, &control)?;
    verify_reclaimed_observations(store, &first, &stale, receipt, deleted.sequence, &control)
}

fn verify_reserved_identifiers(
    store: &dyn VersionedPersistence,
    control: &StorageReadControl,
) -> VersionResult<()> {
    use crate::mvcc::{IdentifierRequest, RECLAMATION_DOMAIN_PREFIX, RECLAMATION_EPOCH_NAMESPACE};
    let before = store.snapshot(control)?.reclamation_epoch();
    let domain = [RECLAMATION_DOMAIN_PREFIX, b"retired/"].concat();
    for namespace in [
        RECLAMATION_EPOCH_NAMESPACE,
        RECLAMATION_DOMAIN_PREFIX,
        &domain,
    ] {
        assert!(matches!(
            store.identifier_watermark(namespace, control),
            Err(VersionError::InvalidEncoding(_))
        ));
        for request in [
            IdentifierRequest::Observe(0),
            IdentifierRequest::Observe(u64::MAX),
            IdentifierRequest::Reserve {
                minimum: 0,
                maximum: u64::MAX,
                count: std::num::NonZeroU64::new(1).unwrap(),
            },
        ] {
            assert!(matches!(
                store.allocate_identifiers(namespace, request, control),
                Err(VersionError::InvalidEncoding(_))
            ));
        }
    }
    let ordinary = b"\0uqa-reclamation-epoch-v1-neighbor\0";
    assert_eq!(
        store
            .allocate_identifiers(ordinary, IdentifierRequest::Observe(7), control)?
            .watermark(),
        7
    );
    assert_eq!(store.identifier_watermark(ordinary, control)?, Some(7));
    assert_eq!(store.snapshot(control)?.reclamation_epoch(), before);
    Ok(())
}

fn verify_reclaimed_observations(
    store: &dyn VersionedPersistence,
    first: &PreparedRecordCommit,
    stale: &PreparedRecordCommit,
    receipt: CommitReceipt,
    through: crate::mvcc::CommitSequence,
    control: &StorageReadControl,
) -> VersionResult<()> {
    let current = store.snapshot(control)?;
    assert_eq!(
        current.sequence(),
        through,
        "physical retirement does not publish a logical commit"
    );
    assert_eq!(current.reclamation_epoch(), Some(1));
    assert!(current.get(b"retired/key", control)?.is_none());
    let untouched = current.get(b"unscoped/key", control)?.unwrap();
    assert_eq!(untouched.sequence(), through);
    assert!(untouched.value().is_none());
    assert_eq!(
        store
            .commit(receipt.transaction, first, control)
            .map_err(rejected)?,
        receipt,
        "original receipt resolution precedes the epoch floor"
    );
    for prepared in [first, stale] {
        let id = store.allocate_transaction(control)?;
        assert!(matches!(
            store.commit(id, prepared, control),
            Err(CommitFailure::Rejected(
                VersionError::ReclaimedObservation { minimum: 1, .. }
            ))
        ));
        assert_eq!(store.abort(id, control)?, CommitStatus::Aborted);
    }
    let writes = [RecordWrite {
        key: b"retired/key",
        expected: None,
        value: Some(b"first"),
    }];
    let fresh = PreparedRecordCommit::new_at_snapshot(&writes, &*current, control)?;
    let fresh_id = store.allocate_transaction(control)?;
    let fresh_receipt = store.commit(fresh_id, &fresh, control).map_err(rejected)?;
    assert_eq!(fresh_receipt.sequence, through.successor()?);
    let positive = PreparedRecordCommit::new(
        &[
            RecordWrite {
                key: b"retired/key",
                expected: Some(fresh_receipt.sequence),
                value: Some(b"observed"),
            },
            RecordWrite {
                key: b"unscoped/fresh",
                expected: None,
                value: Some(b"ordinary"),
            },
        ],
        control,
    )?;
    let positive_id = store.allocate_transaction(control)?;
    store
        .commit(positive_id, &positive, control)
        .map_err(rejected)?;
    drop(current);
    Ok(())
}

/// Verify fixed-cutoff bounded pages on disposable persistence. Sparse keys and a partial final page expose per-key leaks that contiguous range compression can hide.
pub fn verify_tombstone_reclamation_pages(store: &dyn VersionedPersistence) -> VersionResult<()> {
    let control = StorageReadControl::with_limit(1 << 20);
    let keys: Vec<_> = (0..70_u8)
        .map(|index| format!("pages/{index:03}/unique-{}", 701 * usize::from(index)).into_bytes())
        .collect();
    let deleted = seed_pages(store, &keys, &control)?;
    store.reclaim_versions(&control)?;
    let mut request = TombstoneReclamationRequest {
        prefix: b"pages/",
        after: None,
        through: deleted.sequence,
    };
    let TombstoneReclamationStep::More { after, removed } =
        store.reclaim_tombstones(&request, &control)?
    else {
        panic!("first page must keep a cursor");
    };
    assert_eq!(removed, 64);
    let epoch = store.snapshot(&control)?.reclamation_epoch().unwrap();
    let recreated = publish(
        store,
        &[
            RecordWrite {
                key: &keys[0],
                expected: None,
                value: Some(b"earlier"),
            },
            RecordWrite {
                key: &keys[69],
                expected: Some(deleted.sequence),
                value: Some(b"recreated"),
            },
            RecordWrite {
                key: b"pages/999/later",
                expected: None,
                value: Some(b"later"),
            },
        ],
        &control,
    )?;
    let later = publish(
        store,
        &[
            RecordWrite {
                key: &keys[0],
                expected: Some(recreated.sequence),
                value: None,
            },
            RecordWrite {
                key: b"pages/999/later",
                expected: Some(recreated.sequence),
                value: None,
            },
        ],
        &control,
    )?;
    store.reclaim_versions(&control)?;
    request.after = Some(&after);
    assert!(matches!(
        store.reclaim_tombstones(&request, &control)?,
        TombstoneReclamationStep::Complete { removed: 5 }
    ));
    let snapshot = store.snapshot(&control)?;
    assert_eq!(snapshot.reclamation_epoch(), Some(epoch + 1));
    assert_eq!(
        snapshot.get(&keys[0], &control)?.unwrap().sequence(),
        later.sequence
    );
    assert_eq!(
        snapshot
            .get(b"pages/999/later", &control)?
            .unwrap()
            .sequence(),
        later.sequence
    );
    assert_eq!(
        snapshot.get(&keys[69], &control)?.unwrap().sequence(),
        recreated.sequence
    );
    drop(snapshot);
    request.after = None;
    request.through = later.sequence;
    assert!(matches!(
        store.reclaim_tombstones(&request, &control)?,
        TombstoneReclamationStep::Complete { removed: 2 }
    ));
    let snapshot = store.snapshot(&control)?;
    assert_eq!(snapshot.sequence(), later.sequence);
    assert_eq!(snapshot.reclamation_epoch(), Some(epoch + 2));
    for key in keys.iter().take(69) {
        assert!(snapshot.get(key, &control)?.is_none());
    }
    assert!(snapshot.get(b"pages/999/later", &control)?.is_none());
    Ok(())
}

fn seed_pages(
    store: &dyn VersionedPersistence,
    keys: &[Vec<u8>],
    control: &StorageReadControl,
) -> VersionResult<CommitReceipt> {
    let writes: Vec<_> = keys
        .iter()
        .map(|key| RecordWrite {
            key,
            expected: None,
            value: Some(b"payload"),
        })
        .collect();
    let first = publish(store, &writes, control)?;
    let writes: Vec<_> = keys
        .iter()
        .map(|key| RecordWrite {
            key,
            expected: Some(first.sequence),
            value: None,
        })
        .collect();
    publish(store, &writes, control)
}

fn publish(
    store: &dyn VersionedPersistence,
    writes: &[RecordWrite<'_>],
    control: &StorageReadControl,
) -> VersionResult<CommitReceipt> {
    let snapshot = store.snapshot(control)?;
    let prepared = PreparedRecordCommit::new_at_snapshot(writes, &*snapshot, control)?;
    let id = store.allocate_transaction(control)?;
    store.commit(id, &prepared, control).map_err(rejected)
}

fn rejected(error: CommitFailure) -> VersionError {
    match error {
        CommitFailure::Rejected(error) => error,
        error @ CommitFailure::Indeterminate { .. } => {
            VersionError::Storage(crate::StorageBackendError::Other(error.to_string()))
        }
    }
}
