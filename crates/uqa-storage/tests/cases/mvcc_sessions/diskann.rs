//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_storage::diskann_index::pages::DiskANNRecordKey;

#[test]
fn diskann_runtime_reclamation_preserves_private_undo_and_recreation() {
    use uqa_storage::key_value::conformance::{
        verify_diskann_reclamation_bounds, verify_diskann_reclamation_reopen,
        verify_diskann_runtime_reclaimed_reopen, verify_diskann_runtime_reclamation,
    };
    for private in [false, true] {
        let persistence = Persistence::new();
        let store: Arc<dyn KeyValueStore> = Arc::new(persistence.session(1 << 22));
        let generations = verify_diskann_runtime_reclamation(&store, private).unwrap();
        let partial = verify_diskann_reclamation_bounds(&store).unwrap();
        drop(store);
        let store: Arc<dyn KeyValueStore> = Arc::new(persistence.session(1 << 22));
        verify_diskann_runtime_reclaimed_reopen(&store, generations).unwrap();
        verify_diskann_reclamation_reopen(&store, partial).unwrap();
    }
}

#[test]
fn diskann_runtime_retirement_preserves_private_undo_and_recreation() {
    use uqa_storage::key_value::conformance::{
        verify_diskann_runtime_retirement, verify_diskann_runtime_retirement_reopen,
    };
    for private in [false, true] {
        let persistence = Persistence::new();
        let store: Arc<dyn KeyValueStore> = Arc::new(persistence.session(1 << 22));
        let generations = verify_diskann_runtime_retirement(&store, private).unwrap();
        drop(store);
        let store: Arc<dyn KeyValueStore> = Arc::new(persistence.session(1 << 22));
        verify_diskann_runtime_retirement_reopen(&store, generations).unwrap();
    }
}

#[test]
fn diskann_runtime_adoption_rejects_ordinal_gaps_and_conflicting_insertions() {
    let persistence = Persistence::new();
    let store: Arc<dyn KeyValueStore> = Arc::new(persistence.session(1 << 22));
    uqa_storage::key_value::conformance::verify_diskann_runtime_adoption_conflicts(&store).unwrap();
}

#[test]
fn diskann_runtime_lifecycle_preserves_transactions_and_reopen() {
    let persistence = Persistence::new();
    let store: Arc<dyn KeyValueStore> = Arc::new(persistence.session(1 << 22));
    let generation =
        uqa_storage::key_value::conformance::verify_diskann_runtime_lifecycle(&store).unwrap();
    drop(store);
    let store: Arc<dyn KeyValueStore> = Arc::new(persistence.session(1 << 22));
    uqa_storage::key_value::conformance::verify_diskann_runtime_reopen(&store, generation).unwrap();
}

#[test]
fn diskann_live_writes_keep_actual_catalog_visibility_and_reopen() {
    let persistence = Persistence::new();
    let store: Arc<dyn KeyValueStore> = Arc::new(persistence.session(1 << 22));
    let generation =
        uqa_storage::key_value::conformance::verify_diskann_live_writes(&store).unwrap();
    drop(store);
    let reopened: Arc<dyn KeyValueStore> = Arc::new(persistence.session(1 << 22));
    uqa_storage::key_value::conformance::verify_diskann_live_reopen(&reopened, generation).unwrap();
}

#[test]
fn diskann_query_views_retain_private_and_old_committed_generations() {
    let persistence = Persistence::new();
    let store: Arc<dyn KeyValueStore> = Arc::new(persistence.session(1 << 22));
    let generation =
        uqa_storage::key_value::conformance::verify_diskann_query_views(&store).unwrap();
    drop(store);
    let reopened: Arc<dyn KeyValueStore> = Arc::new(persistence.session(1 << 22));
    uqa_storage::key_value::conformance::verify_diskann_query_reopen(&reopened, generation)
        .unwrap();
}

#[test]
fn diskann_publication_is_atomic_on_shared_memory_sessions() {
    let persistence = Persistence::new();
    let store: Arc<dyn KeyValueStore> = Arc::new(persistence.session(1 << 22));
    let generation =
        uqa_storage::key_value::conformance::verify_diskann_publication(&store).unwrap();
    drop(store);
    let reopened: Arc<dyn KeyValueStore> = Arc::new(persistence.session(1 << 22));
    uqa_storage::key_value::conformance::verify_diskann_publication_reopen(&reopened, generation)
        .unwrap();
}
use uqa_storage::key_value::{DiskANNStageStatus, KeyValueDiskANNStore};

#[path = "diskann/identifiers.rs"]
mod identifiers;
#[path = "diskann/identity.rs"]
mod identity;
#[path = "diskann/live.rs"]
mod live;
#[path = "diskann/maintenance.rs"]
mod maintenance;
#[path = "diskann/pruning.rs"]
mod pruning;
#[path = "diskann/publication.rs"]
mod publication;
#[path = "diskann/reclamation.rs"]
mod reclamation;
#[path = "diskann/selection.rs"]
mod selection;

#[test]
fn diskann_generations_preserve_shared_mvcc_and_reopen_contracts() {
    let persistence = Persistence::new();
    let store: Arc<dyn KeyValueStore> = Arc::new(persistence.session(1 << 22));
    let generation =
        uqa_storage::key_value::conformance::verify_diskann_generations(&store).unwrap();
    drop(store);
    let reopened: Arc<dyn KeyValueStore> = Arc::new(persistence.session(1 << 22));
    uqa_storage::key_value::conformance::verify_diskann_reopen(&reopened, generation).unwrap();
}

#[test]
fn diskann_staging_resolves_original_commit_attempt_without_replaying_writes() {
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
        let mut stage = repository.allocate_stage(1, 2, &control).unwrap();
        stage.start(&control).unwrap();
        let start = persistence.state.lock().attempts.len();
        persistence.state.lock().commit_fault = fault;
        assert!(stage
            .write_record(DiskANNRecordKey::Codes(0), b"evaluated", 64, &control)
            .is_err());
        assert!(stage.status(&control).is_err());
        assert!(repository.allocate_stage(1, 2, &control).is_err());
        persistence.state.lock().commit_fault = CommitFault::None;
        repository.commit_pending().unwrap();
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
        drop(history);
        assert_eq!(
            stage.status(&control).unwrap(),
            Some(DiskANNStageStatus::Writing)
        );
        assert!(stage
            .write_record(DiskANNRecordKey::Codes(0), b"replay", 64, &control)
            .is_err());
        assert!(stage.discard_step(2, &control).unwrap());
    }
}

#[test]
fn diskann_freeze_and_discard_fence_a_previously_evaluated_writer() {
    for discard in [false, true] {
        let persistence = Persistence::new();
        let store: Arc<dyn KeyValueStore> = Arc::new(persistence.session(1 << 22));
        let control = StorageReadControl::with_limit(1 << 20);
        let writer = KeyValueDiskANNStore::connect(&store, &control).unwrap();
        writer.initialize(&control).unwrap();
        let mut stage = writer.allocate_stage(1, 2, &control).unwrap();
        stage.start(&control).unwrap();
        let generation = stage.generation();
        persistence.state.lock().commit_fault = CommitFault::LoseBeforeCommit;
        assert!(stage
            .write_record(DiskANNRecordKey::Codes(0), b"late", 64, &control)
            .is_err());

        let other = KeyValueDiskANNStore::connect(&store, &control).unwrap();
        assert!(other.resume_stage(generation, &control).is_err());
        assert!(!other
            .reclaim_abandoned_step(generation, 64, &control)
            .unwrap());
        // Inject a physical metadata change below the staging API. Ordinary peers above cannot steal the pending writer's owner; original record guards must still reject changed state.
        let (state_key, mut state_bytes) = store
            .scan_prefix(b"\0uqa-diskann-v1\0\x01")
            .unwrap()
            .into_iter()
            .find(|(_, value)| value.len() == 18)
            .unwrap();
        assert_eq!(state_bytes[1], 0);
        state_bytes[1] = 1;
        store
            .with_mutation(&mut |_, batch| {
                if discard {
                    batch.delete(&state_key)
                } else {
                    batch.put(&state_key, &state_bytes)
                }
            })
            .unwrap();
        let error = writer.commit_pending().unwrap_err();
        assert!(matches!(
            error.commit_outcome(),
            Some(CommitErrorOutcome::Indeterminate(_))
        ));
        let mut cause: Option<&(dyn std::error::Error + 'static)> = Some(&error);
        let mut read_conflict = false;
        while let Some(error) = cause {
            read_conflict |= matches!(
                error.downcast_ref::<VersionError>(),
                Some(VersionError::ReadConflict { .. })
            );
            cause = error.source();
        }
        assert!(
            read_conflict,
            "retained outcome must keep its typed fence conflict: {error}"
        );
        writer.rollback_pending().unwrap();
        assert_eq!(
            stage.status(&control).unwrap(),
            (!discard).then_some(DiskANNStageStatus::Frozen)
        );
    }
}

#[test]
fn diskann_lost_start_reply_keeps_reserved_identity_and_independent_caller_state() {
    let persistence = Persistence::new();
    let store: Arc<dyn KeyValueStore> = Arc::new(persistence.session(1 << 22));
    let control = StorageReadControl::with_limit(1 << 20);
    let repository = KeyValueDiskANNStore::connect(&store, &control).unwrap();
    repository.initialize(&control).unwrap();
    let mut stage = repository.allocate_stage(1, 2, &control).unwrap();
    let generation = stage.generation();
    persistence.state.lock().commit_fault = CommitFault::LoseReply;
    assert!(stage.start(&control).is_err());
    assert!(!store.in_transaction());
    repository.commit_pending().unwrap();
    stage.start(&control).unwrap();
    assert_eq!(stage.generation(), generation);
    let mut resumed = repository.resume_stage(generation, &control).unwrap();
    assert!(resumed.discard_step(1, &control).unwrap());
    assert!(stage.start(&control).is_err());
}

#[test]
fn diskann_pruning_preserves_committed_and_retained_session_views() {
    let persistence = Persistence::new();
    let store: Arc<dyn KeyValueStore> = Arc::new(persistence.session(1 << 22));
    let generation = uqa_storage::key_value::conformance::verify_diskann_pruning(&store).unwrap();
    uqa_storage::key_value::conformance::verify_diskann_pruning_reopen(&store, generation).unwrap();
}

#[test]
fn diskann_maintenance_source_preserves_census_builds_and_reopen() {
    let persistence = Persistence::new();
    let generation = {
        let store: Arc<dyn KeyValueStore> = Arc::new(persistence.session(1 << 22));
        let backend = uqa_storage::key_value::KeyValueStorageBackend::new(store);
        uqa_storage::key_value::conformance::verify_diskann_maintenance_source(&backend).unwrap()
    };
    let store: Arc<dyn KeyValueStore> = Arc::new(persistence.session(1 << 22));
    let backend = uqa_storage::key_value::KeyValueStorageBackend::new(store);
    uqa_storage::key_value::conformance::verify_diskann_maintenance_reopen(&backend, generation)
        .unwrap();
}

#[test]
fn diskann_build_ownership_protects_live_and_retained_sources() {
    let persistence = Persistence::new();
    let store: Arc<dyn KeyValueStore> = Arc::new(persistence.session(1 << 22));
    uqa_storage::key_value::conformance::verify_diskann_build_ownership(&store).unwrap();
    uqa_storage::key_value::conformance::verify_diskann_publication_ownership(&store).unwrap();
}
