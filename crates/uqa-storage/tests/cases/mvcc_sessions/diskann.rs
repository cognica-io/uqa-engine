//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_storage::diskann_index::pages::DiskANNRecordKey;

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

#[path = "diskann/identity.rs"]
mod identity;
#[path = "diskann/live.rs"]
mod live;
#[path = "diskann/pruning.rs"]
mod pruning;
#[path = "diskann/publication.rs"]
mod publication;
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
    use uqa_storage::diskann_index::format::{
        DiskANNArtifactDigests, DiskANNCoverageBuilder, DiskANNManifest, DiskANNManifestInput,
    };
    use uqa_storage::vector_index::DiskANNIndexParams;

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
        let mut resumed = other.resume_stage(generation, &control).unwrap();
        if discard {
            assert!(resumed.discard_step(1, &control).unwrap());
        } else {
            let manifest = DiskANNManifest::new(DiskANNManifestInput {
                generation,
                dimensions: 2,
                parameters: DiskANNIndexParams::for_dimensions(2).unwrap(),
                nodes: 0,
                side_vectors: 0,
                entry_node: None,
                coverage: DiskANNCoverageBuilder::new(generation, 2).unwrap().finish(),
                artifacts: DiskANNArtifactDigests::empty(),
            })
            .unwrap();
            resumed.seal(manifest, 4096, &control).unwrap();
        }
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
            (!discard).then_some(DiskANNStageStatus::Sealed)
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
