//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use parking_lot::Mutex;
use std::collections::VecDeque;
use uqa_storage::{
    diskann_index::changes::{DiskANNJournalPruner, DiskANNPruneRequest, DiskANNPruneResult},
    mvcc::{
        DatabaseId, StorageTransactionId, TransactionCompletionError, TransactionOutcome,
        TransactionOutcomeId,
    },
    StorageSavepointId,
};

enum RollbackFailure {
    Active,
    Closed,
}

struct State {
    active: bool,
    evaluations: usize,
    begins: usize,
    commits: usize,
    rollbacks: usize,
    failures: VecDeque<Option<TransactionOutcome>>,
    reject_page: bool,
    removed: usize,
    rollback_failures: VecDeque<RollbackFailure>,
    close_on_error: bool,
}

struct Backend(Mutex<State>);

impl Backend {
    fn new(failures: Vec<Option<TransactionOutcome>>, reject_page: bool) -> Arc<Self> {
        Arc::new(Self(Mutex::new(State {
            active: false,
            evaluations: 0,
            begins: 0,
            commits: 0,
            rollbacks: 0,
            failures: failures.into(),
            reject_page,
            removed: 2,
            rollback_failures: VecDeque::new(),
            close_on_error: false,
        })))
    }
}

struct Pruner(Arc<Backend>);

impl DiskANNJournalPruner for Pruner {
    fn prune(
        &self,
        request: DiskANNPruneRequest,
        _: &StorageReadControl,
    ) -> StorageBackendResult<DiskANNPruneResult> {
        let mut state = self.0 .0.lock();
        assert!(state.active);
        state.evaluations += 1;
        assert_eq!(request.max_records, 64);
        assert!(request.after.is_none());
        if state.reject_page {
            return Err(StorageBackendError::Other("rejected page".into()));
        }
        Ok(DiskANNPruneResult {
            examined: 3,
            removed: state.removed,
            next: None,
        })
    }
}

impl PersistentStorageBackend for Backend {
    fn document_store(&self, _: &str) -> Box<dyn uqa_storage::DocumentStore> {
        unreachable!()
    }
    fn inverted_index(
        &self,
        _: &str,
        _: uqa_analysis::Analyzer,
    ) -> Box<dyn uqa_storage::InvertedIndex> {
        unreachable!()
    }
    fn vector_index(
        &self,
        _: &str,
        _: &str,
        _: u32,
        _: uqa_storage::VectorIndexSpec,
        _: uqa_storage::VectorIndexOpenMode,
    ) -> StorageBackendResult<Box<dyn uqa_storage::VectorIndex>> {
        unreachable!()
    }
    fn begin_transaction(&self) -> StorageBackendResult<()> {
        let mut state = self.0.lock();
        assert!(!state.active);
        state.active = true;
        state.begins += 1;
        Ok(())
    }
    fn in_transaction(&self) -> bool {
        self.0.lock().active
    }
    fn transaction_has_written(&self) -> StorageBackendResult<bool> {
        Ok(self.0.lock().evaluations != 0)
    }
    fn commit_transaction(&self) -> StorageBackendResult<()> {
        let mut state = self.0.lock();
        assert!(state.active);
        state.commits += 1;
        if let Some(failure) = state.failures.pop_front() {
            if state.close_on_error {
                state.active = false;
            }
            let error = StorageBackendError::Other("commit reply failure".into());
            return Err(failure.map_or(error, |outcome| {
                StorageBackendError::backend(
                    "fixture",
                    TransactionCompletionError {
                        outcome,
                        publication: None,
                        source: StorageBackendError::Other("reply failure".into()),
                    },
                )
            }));
        }
        state.active = false;
        Ok(())
    }
    fn rollback_transaction(&self) -> StorageBackendResult<()> {
        let mut state = self.0.lock();
        assert!(state.active);
        state.rollbacks += 1;
        if let Some(failure) = state.rollback_failures.pop_front() {
            state.active = matches!(failure, RollbackFailure::Active);
            return Err(StorageBackendError::Other(
                "rollback cleanup failure".into(),
            ));
        }
        state.active = false;
        Ok(())
    }
    fn savepoint(&self, _: StorageSavepointId) -> StorageBackendResult<()> {
        unreachable!()
    }
    fn release_savepoint(&self, _: StorageSavepointId) -> StorageBackendResult<()> {
        unreachable!()
    }
    fn rollback_to_savepoint(&self, _: StorageSavepointId) -> StorageBackendResult<()> {
        unreachable!()
    }
}

fn transaction() -> TransactionOutcomeId {
    TransactionOutcomeId::Records(
        StorageTransactionId::new(DatabaseId::from_bytes([7; 16]), 1).unwrap(),
    )
}

#[test]
fn diskann_maintenance_resolves_original_commit_without_replaying_even_after_cancellation() {
    for outcome in [
        TransactionOutcome::Indeterminate(transaction()),
        TransactionOutcome::Committed(transaction()),
    ] {
        let backend = Backend::new(vec![Some(outcome), Some(outcome)], false);
        let mut job = Job::new(backend.clone(), Box::new(Pruner(backend.clone())));
        let control = StorageReadControl::with_limit(1 << 20);
        assert!(job.step(&control).is_err());
        assert!(job.pending());
        assert!(job.take_completed().is_none());
        control.cancellation().cancel();
        assert!(job.step(&control).is_err());
        assert!(job.take_completed().is_none());
        job.step(&control).unwrap();
        assert!(job.finished());
        assert_eq!(job.take_completed().unwrap().removed, 2);
        let state = backend.0.lock();
        assert_eq!(
            (
                state.begins,
                state.evaluations,
                state.commits,
                state.rollbacks
            ),
            (1, 1, 3, 0)
        );
    }
}

#[test]
fn diskann_maintenance_failed_pages_and_definite_commit_failures_never_advance() {
    for (failures, reject_page) in [
        (vec![], true),
        (vec![None], false),
        (
            vec![Some(TransactionOutcome::Aborted(transaction()))],
            false,
        ),
    ] {
        let backend = Backend::new(failures, reject_page);
        let mut job = Job::new(backend.clone(), Box::new(Pruner(backend.clone())));
        let control = StorageReadControl::with_limit(1 << 20);
        assert!(job.step(&control).is_err());
        assert!(job.take_completed().is_none());
        if job.pending() {
            job.step(&control).unwrap();
        }
        assert!(job.finished());
        assert!(job.take_completed().is_none());
        let state = backend.0.lock();
        assert_eq!(
            (state.begins, state.evaluations, state.rollbacks),
            (1, 1, 1)
        );
    }
}

#[test]
fn diskann_maintenance_read_only_pages_wait_for_cleanup_without_publishing() {
    let backend = Backend::new(vec![], false);
    {
        let mut state = backend.0.lock();
        state.removed = 0;
        state.rollback_failures.push_back(RollbackFailure::Active);
    }
    let mut job = Job::new(backend.clone(), Box::new(Pruner(backend.clone())));
    let control = StorageReadControl::with_limit(1 << 20);
    assert!(job.step(&control).is_err());
    assert!(job.pending());
    assert!(job.take_completed().is_none());
    control.cancellation().cancel();
    job.step(&control).unwrap();
    assert_eq!(job.take_completed().unwrap().removed, 0);
    assert!(job.finished());
    let state = backend.0.lock();
    assert_eq!(
        (state.evaluations, state.commits, state.rollbacks),
        (1, 0, 2)
    );
}

#[test]
fn diskann_maintenance_records_confirmed_commit_after_session_cleanup_error() {
    let backend = Backend::new(
        vec![Some(TransactionOutcome::Committed(transaction()))],
        false,
    );
    backend.0.lock().close_on_error = true;
    let mut job = Job::new(backend.clone(), Box::new(Pruner(backend)));
    assert!(job.step(&StorageReadControl::with_limit(1 << 20)).is_err());
    assert!(job.finished());
    assert!(!job.pending());
    assert_eq!(job.take_completed().unwrap().removed, 2);
}

#[test]
fn diskann_maintenance_page_errors_survive_rollback_failures() {
    for closes in [false, true] {
        let backend = Backend::new(vec![], true);
        {
            let mut state = backend.0.lock();
            state.rollback_failures.push_back(if closes {
                RollbackFailure::Closed
            } else {
                RollbackFailure::Active
            });
        }
        let mut job = Job::new(backend.clone(), Box::new(Pruner(backend.clone())));
        let control = StorageReadControl::with_limit(1 << 20);
        assert!(
            matches!(job.step(&control), Err(StorageBackendError::Other(message)) if message == "rejected page")
        );
        assert_eq!(job.finished(), closes);
        assert_eq!(job.pending(), !closes);
        assert!(job.take_completed().is_none());
        control.cancellation().cancel();
        job.step(&control).unwrap();
        assert!(job.finished());
        assert!(!job.pending());
        assert!(job.take_completed().is_none());
        let state = backend.0.lock();
        assert_eq!(
            (
                state.begins,
                state.evaluations,
                state.commits,
                state.rollbacks
            ),
            (1, 1, 0, if closes { 1 } else { 2 })
        );
    }
}

#[test]
fn diskann_maintenance_closed_rollback_failure_releases_the_original_attempt() {
    for failure in [None, Some(TransactionOutcome::Aborted(transaction()))] {
        let backend = Backend::new(vec![failure], false);
        {
            let mut state = backend.0.lock();
            state.rollback_failures.push_back(RollbackFailure::Closed);
        }
        let mut job = Job::new(backend.clone(), Box::new(Pruner(backend.clone())));
        let control = StorageReadControl::with_limit(1 << 20);
        assert!(job.step(&control).is_err());
        assert!(job.pending());
        assert!(job.take_completed().is_none());
        control.cancellation().cancel();
        assert!(
            matches!(job.step(&control), Err(StorageBackendError::Other(message)) if message == "rollback cleanup failure")
        );
        assert!(job.finished());
        assert!(!job.pending());
        job.step(&control).unwrap();
        assert!(job.take_completed().is_none());
        let state = backend.0.lock();
        assert_eq!(
            (
                state.begins,
                state.evaluations,
                state.commits,
                state.rollbacks
            ),
            (1, 1, 1, 1)
        );
    }
}
