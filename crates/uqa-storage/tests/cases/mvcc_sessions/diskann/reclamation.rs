//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_storage::key_value::conformance::{
    verify_diskann_runtime_reclaimed_reopen, verify_diskann_runtime_retirement,
};

#[test]
fn diskann_legacy_owner_transition_remains_exclusive_before_its_new_identity_commits() {
    for reclaim in [false, true] {
        let persistence = Persistence::new();
        let store: Arc<dyn KeyValueStore> = Arc::new(persistence.session(1 << 22));
        let control = StorageReadControl::with_limit(1 << 20);
        let repository = KeyValueDiskANNStore::connect(&store, &control).unwrap();
        repository.initialize(&control).unwrap();
        let mut stage = repository.allocate_stage(17, 18, &control).unwrap();
        stage.start(&control).unwrap();
        let generation = stage.generation();
        drop(stage);
        let (key, mut bytes) = store
            .scan_prefix(b"\0uqa-diskann-v1\0\x01")
            .unwrap()
            .into_iter()
            .find(|(_, value)| value.len() == 18)
            .unwrap();
        bytes[0] = 1;
        bytes[2..].fill(9);
        store
            .with_mutation(&mut |_, batch| batch.put(&key, &bytes))
            .unwrap();
        let peer = KeyValueDiskANNStore::connect(&store, &control).unwrap();
        persistence.state.lock().commit_fault = CommitFault::LoseBeforeCommit;
        if reclaim {
            assert!(repository
                .reclaim_abandoned_step(generation, 64, &control)
                .is_err());
        } else {
            assert!(repository.resume_stage(generation, &control).is_err());
        }
        assert_eq!(store.get(&key).unwrap().unwrap(), bytes);
        assert!(!peer
            .reclaim_abandoned_step(generation, 64, &control)
            .unwrap());
        assert!(peer.resume_stage(generation, &control).is_err());
        persistence.state.lock().commit_fault = CommitFault::None;
        repository.commit_pending().unwrap();
        if !reclaim {
            let resumed = peer.resume_stage(generation, &control).unwrap();
            assert_eq!(store.get(&key).unwrap().unwrap()[0], 2);
            drop(resumed);
        }
        assert!(peer
            .reclaim_abandoned_step(generation, 64, &control)
            .unwrap());
    }
}

#[test]
fn diskann_pending_build_attempt_retains_ownership_after_the_stage_closes() {
    use uqa_storage::diskann_index::pages::DiskANNRecordKey;
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
        let mut stage = repository.allocate_stage(17, 18, &control).unwrap();
        stage.start(&control).unwrap();
        let generation = stage.generation();
        let peer = KeyValueDiskANNStore::connect(&store, &control).unwrap();
        persistence.state.lock().commit_fault = fault;
        assert!(stage
            .write_record(DiskANNRecordKey::Codes(0), b"original", 64, &control)
            .is_err());
        drop(stage);
        assert!(!peer
            .reclaim_abandoned_step(generation, 64, &control)
            .unwrap());
        assert!(peer.resume_stage(generation, &control).is_err());
        persistence.state.lock().commit_fault = CommitFault::None;
        repository.commit_pending().unwrap();
        assert!(peer
            .reclaim_abandoned_step(generation, 64, &control)
            .unwrap());
    }
}

#[test]
fn diskann_pending_abandoned_cleanup_excludes_new_sources_and_resolves_original_bytes() {
    for fault in [
        CommitFault::LoseReply,
        CommitFault::LoseBeforeCommit,
        CommitFault::Reject,
    ] {
        let persistence = Persistence::new();
        let store: Arc<dyn KeyValueStore> = Arc::new(persistence.session(1 << 22));
        let generation =
            uqa_storage::key_value::conformance::verify_diskann_generations(&store).unwrap();
        let control = StorageReadControl::with_limit(1 << 20);
        let repository = KeyValueDiskANNStore::connect(&store, &control).unwrap();
        let peer = KeyValueDiskANNStore::connect(&store, &control).unwrap();
        let start = persistence.state.lock().attempts.len();
        persistence.state.lock().commit_fault = fault;
        assert!(repository
            .reclaim_abandoned_step(generation, 1, &control)
            .is_err());
        assert!(peer.open_source(generation, &control).is_err());
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
        assert!(peer
            .reclaim_abandoned_step(generation, 64, &control)
            .unwrap());
    }
}

#[test]
fn diskann_reclamation_resolves_the_original_deletions_after_lost_commit_replies() {
    for fault in [
        CommitFault::LoseReply,
        CommitFault::LoseBeforeCommit,
        CommitFault::Reject,
    ] {
        let persistence = Persistence::new();
        let store: Arc<dyn KeyValueStore> = Arc::new(persistence.session(1 << 22));
        let generations = verify_diskann_runtime_retirement(&store, false).unwrap();
        let control = StorageReadControl::with_limit(1 << 20);
        let repository = KeyValueDiskANNStore::connect(&store, &control).unwrap();
        let start = persistence.state.lock().attempts.len();
        persistence.state.lock().commit_fault = fault;
        assert!(repository
            .reclaim_retired_step(generations.0, 1, &control)
            .is_err());
        assert!(repository
            .reclaim_retired_step(generations.0, 64, &control)
            .is_err());
        persistence.state.lock().commit_fault = CommitFault::None;
        repository.commit_pending().unwrap();
        {
            let history = persistence.state.lock();
            assert_eq!(history.attempts[start], *history.attempts.last().unwrap());
            assert_eq!(
                history.attempts.len() - start,
                if fault == CommitFault::LoseBeforeCommit {
                    2
                } else {
                    1
                },
            );
        }
        assert_eq!(
            repository
                .resume_stage(generations.0, &control)
                .unwrap()
                .status(&control)
                .unwrap(),
            Some(DiskANNStageStatus::Discarding),
        );
        assert!(repository
            .reclaim_retired_step(generations.0, 64, &control)
            .unwrap());
        verify_diskann_runtime_reclaimed_reopen(&store, generations).unwrap();
    }
}
