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
