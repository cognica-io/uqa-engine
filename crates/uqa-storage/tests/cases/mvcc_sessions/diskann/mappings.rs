//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_storage::key_value::{KeyValueDiskANNMaintenance, KeyValueDiskANNMappingMaintenance};

#[path = "mappings/validation.rs"]
mod validation;

fn retired(
    store: &Arc<dyn KeyValueStore>,
    control: &StorageReadControl,
) -> (
    KeyValueDiskANNStore,
    uqa_storage::diskann_index::catalog::DiskANNIndexScope,
    uqa_storage::diskann_index::format::DiskANNGeneration,
) {
    let scope = super::identity::scope(store, control);
    let repository = KeyValueDiskANNStore::connect(store, control).unwrap();
    repository.initialize(control).unwrap();
    let stage = repository.allocate_bound_stage(&scope, control).unwrap();
    let generation = stage.generation();
    drop(stage);
    assert!(repository
        .reclaim_abandoned_step(generation, 64, control)
        .unwrap());
    (repository, scope, generation)
}

#[test]
fn diskann_mapping_cleanup_resolves_original_lost_outcomes_without_advancing_its_cursor() {
    for fault in [
        CommitFault::LoseReply,
        CommitFault::LoseBeforeCommit,
        CommitFault::Reject,
    ] {
        let persistence = Persistence::new();
        let store: Arc<dyn KeyValueStore> = Arc::new(persistence.session(1 << 22));
        let control = StorageReadControl::with_limit(1 << 20);
        let (_repository, _scope, _generation) = retired(&store, &control);
        let mut pass = KeyValueDiskANNMappingMaintenance::start(&store, &control).unwrap();
        let before = persistence.state.lock().attempts.len();
        persistence.state.lock().commit_fault = fault;
        assert!(pass.step().is_err());
        assert!(pass.step().is_err());
        persistence.state.lock().commit_fault = CommitFault::None;
        pass.commit_pending().unwrap();
        let state = persistence.state.lock();
        assert_eq!(state.attempts[before], *state.attempts.last().unwrap());
        assert_eq!(
            state.attempts.len() - before,
            if fault == CommitFault::LoseBeforeCommit {
                2
            } else {
                1
            }
        );
        drop(state);
        assert_eq!(pass.step().unwrap(), Some(true));
        assert_eq!(pass.step().unwrap(), Some(true));
        assert_eq!(pass.step().unwrap(), None);
    }
}

#[test]
fn diskann_mapping_cleanup_cannot_delete_an_index_or_table_prepared_after_its_read() {
    for table in [false, true] {
        let persistence = Persistence::new();
        let store: Arc<dyn KeyValueStore> = Arc::new(persistence.session(1 << 22));
        let control = StorageReadControl::with_limit(1 << 20);
        let (repository, scope, original) = retired(&store, &control);
        let mut pass = KeyValueDiskANNMappingMaintenance::start(&store, &control).unwrap();
        if table {
            assert_eq!(pass.step().unwrap(), Some(true));
        }
        persistence.state.lock().commit_fault = CommitFault::LoseBeforeCommit;
        assert!(pass.step().is_err());
        let replacement = repository.allocate_bound_stage(&scope, &control).unwrap();
        assert_eq!(replacement.generation().table(), original.table());
        if table {
            assert!(replacement.generation().index() > original.index());
        } else {
            assert_eq!(replacement.generation().index(), original.index());
        }
        assert!(pass.commit_pending().is_err());
        pass.rollback_pending().unwrap();
        assert_eq!(pass.step().unwrap(), Some(false));
        while let Some(reclaimed) = pass.step().unwrap() {
            assert!(!reclaimed);
        }
        assert_eq!(
            replacement.status(&control).unwrap(),
            Some(DiskANNStageStatus::Writing)
        );
        drop(replacement);
        KeyValueDiskANNMaintenance::run(&store, &control).unwrap();
        let stage = repository.allocate_bound_stage(&scope, &control).unwrap();
        assert!(stage.generation().table() > original.table());
    }
}

#[test]
fn diskann_bound_allocation_retries_only_rejected_cleanup_conflicts_with_fresh_identifiers() {
    let persistence = Persistence::new();
    let store: Arc<dyn KeyValueStore> = Arc::new(persistence.session(1 << 22));
    let control = StorageReadControl::with_limit(1 << 20);
    let (repository, scope, original) = retired(&store, &control);
    let before = persistence.state.lock().attempts.len();
    persistence.state.lock().commit_fault = CommitFault::ReclaimDiskANNMapping;
    let stage = repository.allocate_bound_stage(&scope, &control).unwrap();
    assert_eq!(stage.generation().table(), original.table());
    assert!(stage.generation().index() > original.index());
    assert!(stage.generation().generation() > original.generation() + 1);
    let commits = persistence.state.lock();
    assert_eq!(commits.attempts.len() - before, 2);
    assert_ne!(commits.attempts[before], commits.attempts[before + 1]);
    drop(commits);
    assert_eq!(
        stage.status(&control).unwrap(),
        Some(DiskANNStageStatus::Writing)
    );
}
