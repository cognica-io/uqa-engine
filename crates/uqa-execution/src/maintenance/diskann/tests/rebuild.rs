//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::maintenance::diskann::rebuild::Configuration;
use uqa_storage::diskann_index::{
    format::DiskANNGeneration,
    maintenance::{
        DiskANNChangeStatistics, DiskANNMaintenanceSource, DiskANNStatisticsPage,
        DiskANNStatisticsRequest,
    },
};

struct Source {
    backend: Arc<Backend>,
    changes: DiskANNChangeStatistics,
}

impl DiskANNJournalPruner for Source {
    fn prune(
        &self,
        request: DiskANNPruneRequest,
        control: &StorageReadControl,
    ) -> StorageBackendResult<DiskANNPruneResult> {
        Pruner(self.backend.clone()).prune(request, control)
    }
}

impl DiskANNMaintenanceSource for Source {
    fn statistics(
        &self,
        request: DiskANNStatisticsRequest,
        control: &StorageReadControl,
    ) -> StorageBackendResult<DiskANNStatisticsPage> {
        control.check()?;
        assert_eq!(request.max_records, 64);
        assert!(request.after.is_none());
        let mut state = self.backend.0.lock();
        assert!(!state.active);
        state.censuses += 1;
        Ok(DiskANNStatisticsPage {
            generation: DiskANNGeneration::new([7; 16], 1, 2, 3)?,
            examined: 3,
            outstanding: self.changes,
            next: None,
        })
    }

    fn rebuild(
        self: Box<Self>,
        _: DiskANNIndexOptions,
        temporary: &DiskANNTemporaryBudget,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        control.check()?;
        assert_eq!(
            temporary.limit(),
            1234,
            "the original host allowance reaches construction"
        );
        let mut state = self.backend.0.lock();
        assert!(state.active);
        state.evaluations += 1;
        state.rebuilds += 1;
        if state.reject_page {
            return Err(StorageBackendError::Other("rejected build".into()));
        }
        Ok(())
    }
}

fn job(
    backend: Arc<Backend>,
    changes: DiskANNChangeStatistics,
    policy: DiskANNRebuildPolicy,
) -> Job {
    Job::with_rebuild(
        backend.clone(),
        Box::new(Source { backend, changes }),
        DiskANNIndexOptions::for_parameters(
            uqa_storage::vector_index::DiskANNIndexParams::for_dimensions(2).unwrap(),
        ),
        Configuration {
            policy,
            temporary: DiskANNTemporaryBudget::new(1234),
        },
    )
}

fn changed() -> DiskANNChangeStatistics {
    DiskANNChangeStatistics {
        documents: 1,
        vectors: 2,
        vector_bytes: 16,
    }
}

#[test]
fn diskann_rebuild_thresholds_preserve_exact_counts_empty_tensors_and_read_only_admission() {
    assert!(DiskANNRebuildPolicy::new(0, 16).is_err());
    assert!(DiskANNRebuildPolicy::new(1, 0).is_err());
    for (changes, documents, bytes, rebuild) in [
        (DiskANNChangeStatistics::default(), 1, 1, false),
        (changed(), 2, 17, false),
        (changed(), 2, 16, true),
        (changed(), 1, 17, true),
        (
            DiskANNChangeStatistics {
                documents: 1,
                vectors: 0,
                vector_bytes: 0,
            },
            1,
            1,
            true,
        ),
    ] {
        let backend = Backend::new(vec![], false);
        let mut job = job(
            backend.clone(),
            changes,
            DiskANNRebuildPolicy::new(documents, bytes).unwrap(),
        );
        let control = StorageReadControl::with_limit(1 << 20);
        assert_eq!(job.phase(), Some(DiskANNMaintenancePhase::Counting));
        job.step(&control).unwrap();
        assert_eq!(job.take_census().unwrap().changes, changes);
        assert_eq!(backend.0.lock().begins, 0);
        assert!(job.take_completed().is_none());
        assert_eq!(
            job.phase(),
            Some(if rebuild {
                DiskANNMaintenancePhase::Rebuilding
            } else {
                DiskANNMaintenancePhase::Pruning
            })
        );
        job.step(&control).unwrap();
        assert!(job.finished());
        assert_eq!(
            job.take_completed(),
            Some(if rebuild {
                Completed::Rebuilt
            } else {
                Completed::Pruned(DiskANNPruneResult {
                    examined: 3,
                    removed: 2,
                    next: None,
                })
            })
        );
        assert_eq!(backend.0.lock().rebuilds, usize::from(rebuild));
    }
}

#[test]
fn diskann_rebuild_resolves_original_commit_without_reconstruction_or_recapture() {
    for outcome in [
        TransactionOutcome::Indeterminate(transaction()),
        TransactionOutcome::Committed(transaction()),
    ] {
        let backend = Backend::new(vec![Some(outcome), Some(outcome)], false);
        let mut job = job(
            backend.clone(),
            changed(),
            DiskANNRebuildPolicy::new(1, 16).unwrap(),
        );
        let control = StorageReadControl::with_limit(1 << 20);
        job.step(&control).unwrap();
        assert!(job.take_census().is_some());
        assert!(job.step(&control).is_err());
        assert!(job.pending());
        assert!(job.take_completed().is_none());
        assert_eq!(job.phase(), Some(DiskANNMaintenancePhase::Completing));
        control.cancellation().cancel();
        assert!(job.step(&control).is_err());
        assert!(job.take_completed().is_none());
        job.step(&control).unwrap();
        assert!(job.finished());
        assert_eq!(job.take_completed(), Some(Completed::Rebuilt));
        assert!(job.take_census().is_none());
        let state = backend.0.lock();
        assert_eq!(
            (
                state.censuses,
                state.rebuilds,
                state.begins,
                state.commits,
                state.rollbacks
            ),
            (1, 1, 1, 3, 0)
        );
    }
}

#[test]
fn diskann_rebuild_failures_keep_original_errors_and_never_report_publication() {
    for closes in [false, true] {
        let backend = Backend::new(vec![], true);
        backend.0.lock().rollback_failures.push_back(if closes {
            RollbackFailure::Closed
        } else {
            RollbackFailure::Active
        });
        let mut job = job(
            backend.clone(),
            changed(),
            DiskANNRebuildPolicy::new(1, 16).unwrap(),
        );
        let control = StorageReadControl::with_limit(1 << 20);
        job.step(&control).unwrap();
        assert!(
            matches!(job.step(&control), Err(StorageBackendError::Other(message)) if message == "rejected build")
        );
        assert_eq!(job.pending(), !closes);
        assert!(job.take_completed().is_none());
        control.cancellation().cancel();
        job.step(&control).unwrap();
        assert!(job.finished());
        assert!(job.take_completed().is_none());
        let state = backend.0.lock();
        assert_eq!(
            (
                state.censuses,
                state.rebuilds,
                state.begins,
                state.commits,
                state.rollbacks
            ),
            (1, 1, 1, 0, if closes { 1 } else { 2 })
        );
    }
}

#[test]
fn diskann_rebuild_rejected_commits_preserve_one_evaluation_and_report_no_success() {
    for outcome in [None, Some(TransactionOutcome::Aborted(transaction()))] {
        for closes in [false, true] {
            let backend = Backend::new(vec![outcome], false);
            backend.0.lock().close_on_error = closes;
            let mut job = job(
                backend.clone(),
                changed(),
                DiskANNRebuildPolicy::new(1, 16).unwrap(),
            );
            let control = StorageReadControl::with_limit(1 << 20);
            job.step(&control).unwrap();
            assert!(job.step(&control).is_err());
            assert!(job.take_completed().is_none());
            control.cancellation().cancel();
            job.step(&control).unwrap();
            assert!(job.finished());
            assert!(job.take_completed().is_none());
            let state = backend.0.lock();
            assert_eq!(
                (
                    state.censuses,
                    state.rebuilds,
                    state.commits,
                    state.rollbacks
                ),
                (1, 1, 1, usize::from(!closes))
            );
        }
    }
}

#[test]
fn diskann_rebuild_honors_cancellation_before_construction_and_confirmed_commit_cleanup() {
    let backend = Backend::new(vec![], false);
    let mut cancelled = job(
        backend.clone(),
        changed(),
        DiskANNRebuildPolicy::new(1, 16).unwrap(),
    );
    let control = StorageReadControl::with_limit(1 << 20);
    cancelled.step(&control).unwrap();
    control.cancellation().cancel();
    assert!(cancelled.step(&control).is_err());
    assert!(cancelled.finished());
    assert!(cancelled.take_completed().is_none());
    {
        let state = backend.0.lock();
        assert_eq!((state.begins, state.rebuilds), (0, 0));
    }

    let backend = Backend::new(
        vec![Some(TransactionOutcome::Committed(transaction()))],
        false,
    );
    backend.0.lock().close_on_error = true;
    let mut committed = job(
        backend.clone(),
        changed(),
        DiskANNRebuildPolicy::new(1, 16).unwrap(),
    );
    let control = StorageReadControl::with_limit(1 << 20);
    committed.step(&control).unwrap();
    assert!(committed.step(&control).is_err());
    assert!(committed.finished());
    assert_eq!(committed.take_completed(), Some(Completed::Rebuilt));
    assert_eq!(backend.0.lock().rebuilds, 1);
}
