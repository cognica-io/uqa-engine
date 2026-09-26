//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    expect, expect_eq, Arc, AtomicBool, Faults, KeyValueStore, Ordering, PreparedRecordCommit,
    StorageBackendResult, StorageReadControl, VersionError, VersionedKeyValueStore,
    VersionedPersistence, VersionedSessionOptions,
};
use crate::key_value::{KeyValueDiskANNCanonical, KeyValueVectorFieldGuards};
use crate::mvcc::{
    CommitFailure, RecordWrite, VectorFieldGuard, VectorFieldGuardLayout,
    VectorFieldGuardMaintenance,
};

/// Preserve original cleanup receipts and finite discovery; reject stale detached observations, quota exhaustion, cancellation and corrupt marker data.
pub fn verify_vector_field_guard_attempts(
    persistence: Arc<dyn VersionedPersistence>,
) -> StorageBackendResult<()> {
    let faults = Arc::new(Faults {
        inner: persistence,
        lose_commit: AtomicBool::new(false),
        fail_abort: AtomicBool::new(false),
        fail_acknowledgement: AtomicBool::new(false),
    });
    let session = Arc::new(VersionedKeyValueStore::new(
        faults.clone(),
        None,
        VersionedSessionOptions::default(),
    ));
    let store: Arc<dyn KeyValueStore> = session.clone();
    let control = StorageReadControl::with_limit(1 << 20);
    let before = faults
        .inner
        .snapshot(&control)
        .map_err(VersionError::into_storage_error)?;
    let canonical = KeyValueDiskANNCanonical::new(store.clone(), "guard-attempt", "field", 2)?;
    canonical.replace(0, &[], &control)?;
    let prefix = KeyValueVectorFieldGuards
        .prefix(&control)
        .map_err(VersionError::into_storage_error)?;
    let key = store.scan_prefix(&prefix)?[0].0.clone();
    let guard = KeyValueVectorFieldGuards
        .reference(&key, &control)
        .map_err(VersionError::into_storage_error)?
        .unwrap();
    let old_absence = PreparedRecordCommit::new_at_snapshot(
        &[RecordWrite {
            key: &guard.lifetime,
            expected: None,
            value: None,
        }],
        &*before,
        &control,
    )
    .map_err(VersionError::into_storage_error)?;
    drop(before);
    let mut pass = VectorFieldGuardMaintenance::start(&session, &control)?;
    faults.lose_commit.store(true, Ordering::SeqCst);
    expect(
        pass.step().is_err(),
        "lost cleanup reply retains the original step",
    )?;
    expect(pass.step().is_err(), "unresolved cleanup cannot advance")?;
    let committed = faults
        .inner
        .snapshot(&control)
        .map_err(VersionError::into_storage_error)?
        .sequence();
    canonical.replace(0, &[], &control)?;
    pass.commit_pending()?;
    expect_eq(
        &pass.step()?,
        &Some(true),
        "receipt completion advances the original field once",
    )?;
    expect(
        store.get(&guard.references)?.is_some(),
        "receipt retry does not erase a later marker",
    )?;
    expect_eq(
        &pass.step()?,
        &None,
        "new guard identities do not extend discovery",
    )?;
    drop(pass);
    expect(
        faults
            .inner
            .snapshot(&control)
            .map_err(VersionError::into_storage_error)?
            .sequence()
            > committed,
        "intervening writer remains committed",
    )?;
    store.reclaim_obsolete()?;
    let id = faults
        .inner
        .allocate_transaction(&control)
        .map_err(VersionError::into_storage_error)?;
    expect(
        matches!(
            faults.inner.commit(id, &old_absence, &control),
            Err(CommitFailure::Rejected(
                VersionError::ReclaimedObservation { .. }
            ))
        ),
        "guard retirement rejects a detached original absence",
    )?;
    faults
        .inner
        .abort(id, &control)
        .map_err(VersionError::into_storage_error)?;
    verify_failure_controls(&session, &store, &canonical, &guard, &control)
}

fn verify_failure_controls(
    session: &VersionedKeyValueStore,
    store: &Arc<dyn KeyValueStore>,
    canonical: &KeyValueDiskANNCanonical,
    guard: &VectorFieldGuard,
    control: &StorageReadControl,
) -> StorageBackendResult<()> {
    let tiny = StorageReadControl::with_limit(0);
    expect(
        VectorFieldGuardMaintenance::start(session, &tiny).is_err(),
        "guard discovery reserves its workspace",
    )?;
    let cancelled = StorageReadControl::with_limit(1 << 20);
    cancelled.cancellation().cancel();
    expect(
        VectorFieldGuardMaintenance::start(session, &cancelled).is_err(),
        "guard discovery checks cancellation",
    )?;
    canonical.replace(0, &[], control)?;
    store.put(&guard.references, b"corrupt")?;
    let mut malformed = VectorFieldGuardMaintenance::start(session, control)?;
    expect(
        malformed.step().is_err(),
        "changed immutable marker rejects cleanup",
    )?;
    expect(
        malformed.step().is_err(),
        "a failed step cannot silently skip corrupt data",
    )?;
    expect_eq(
        &store.get(&guard.references)?,
        &Some(b"corrupt".to_vec()),
        "failed cleanup does not delete corrupt input",
    )?;
    drop(malformed);
    store.put(&guard.references, &guard.reference_value)?;
    store.reclaim_obsolete()
}
