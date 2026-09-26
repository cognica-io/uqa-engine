//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_storage::key_value::{DiskANNMaintenanceStatus, KeyValueDiskANNMaintenance};

#[test]
fn diskann_maintenance_uses_finite_key_only_discovery_and_vacuum() {
    let persistence = Persistence::new();
    let store: Arc<dyn KeyValueStore> = Arc::new(persistence.session(1 << 22));
    uqa_storage::key_value::conformance::verify_diskann_maintenance(&store).unwrap();
}

#[test]
fn diskann_maintenance_reclaims_retirement_without_discarding_current_heads() {
    use uqa_storage::key_value::conformance::{
        verify_diskann_runtime_reclaimed_reopen, verify_diskann_runtime_retirement,
    };
    for private in [false, true] {
        let persistence = Persistence::new();
        let store: Arc<dyn KeyValueStore> = Arc::new(persistence.session(1 << 22));
        let generations = verify_diskann_runtime_retirement(&store, private).unwrap();
        store.vacuum().unwrap();
        verify_diskann_runtime_reclaimed_reopen(&store, generations).unwrap();
    }
}

#[test]
fn diskann_maintenance_preserves_original_attempts_and_cursor_after_lost_replies() {
    for fault in [
        CommitFault::LoseReply,
        CommitFault::LoseBeforeCommit,
        CommitFault::Reject,
    ] {
        let persistence = Persistence::new();
        let store: Arc<dyn KeyValueStore> = Arc::new(persistence.session(1 << 22));
        let control = StorageReadControl::with_limit(1 << 20);
        let repository = KeyValueDiskANNStore::connect(&store, &control).unwrap();
        repository.initialize(&control).unwrap();
        let mut stage = repository.allocate_stage(11, 12, &control).unwrap();
        stage.start(&control).unwrap();
        stage
            .write_record(DiskANNRecordKey::Codes(0), b"retained", 64, &control)
            .unwrap();
        let generation = stage.generation();
        drop(stage);
        let mut pass = KeyValueDiskANNMaintenance::start(&store, &control).unwrap();
        let start = persistence.state.lock().attempts.len();
        persistence.state.lock().commit_fault = fault;
        assert!(pass.step().is_err());
        assert!(pass.step().is_err());
        if fault == CommitFault::LoseBeforeCommit {
            assert!(!repository
                .reclaim_abandoned_step(generation, 64, &control)
                .unwrap());
        }
        persistence.state.lock().commit_fault = CommitFault::None;
        pass.commit_pending().unwrap();
        {
            let history = persistence.state.lock();
            assert_eq!(history.attempts[start], *history.attempts.last().unwrap());
            assert_eq!(
                history.attempts.len() - start,
                if fault == CommitFault::LoseBeforeCommit {
                    2
                } else {
                    1
                }
            );
        }
        let result = pass.step().unwrap().unwrap();
        assert_eq!(result.generation, generation);
        assert_eq!(result.status, DiskANNMaintenanceStatus::Reclaimed);
        assert!(pass.step().unwrap().is_none());
        assert!(repository.resume_stage(generation, &control).is_err());
    }
}

#[test]
fn diskann_maintenance_respects_empty_storage_cancellation_and_original_transactions() {
    let persistence = Persistence::new();
    let store: Arc<dyn KeyValueStore> = Arc::new(persistence.session(1 << 22));
    let control = StorageReadControl::with_limit(8192);
    let start = persistence.state.lock().attempts.len();
    let mut empty = KeyValueDiskANNMaintenance::start(&store, &control).unwrap();
    assert!(empty.step().unwrap().is_none());
    drop(empty);
    assert_eq!(persistence.state.lock().attempts.len(), start);
    assert_eq!(control.memory().used(), 0);
    assert!(KeyValueDiskANNMaintenance::start(&store, &StorageReadControl::with_limit(0)).is_err());
    let repository = KeyValueDiskANNStore::connect(&store, &control).unwrap();
    repository.initialize(&control).unwrap();
    let mut stage = repository.allocate_stage(11, 12, &control).unwrap();
    stage.start(&control).unwrap();
    let generation = stage.generation();
    drop(stage);
    let cancelled = StorageReadControl::with_limit(8192);
    let mut pass = KeyValueDiskANNMaintenance::start(&store, &cancelled).unwrap();
    cancelled.cancellation().cancel();
    assert!(matches!(
        pass.step(),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert!(repository.resume_stage(generation, &control).is_ok());
    drop(pass);
    assert_eq!(cancelled.memory().used(), 0);
    store.begin_transaction().unwrap();
    store.put(b"caller-private", b"kept").unwrap();
    assert!(store.vacuum().is_err());
    assert!(store.in_transaction());
    assert_eq!(
        store.get(b"caller-private").unwrap(),
        Some(b"kept".to_vec())
    );
    store.rollback_transaction().unwrap();
    store.vacuum().unwrap();
    assert!(repository.resume_stage(generation, &control).is_err());
}
