//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Preserve one evaluated pruning page or rebuild until its original transaction completes.

use super::rebuild::{Capture, Configuration, DiskANNMaintenanceCensus};
use super::DiskANNMaintenancePhase;
use std::sync::Arc;
use uqa_storage::{
    diskann_index::{
        changes::{
            DiskANNJournalPruner, DiskANNPruneCursor, DiskANNPruneRequest, DiskANNPruneResult,
        },
        maintenance::DiskANNMaintenanceSource,
        DiskANNIndexOptions,
    },
    mvcc::TransactionOutcome,
    read_control::StorageReadControl,
    PersistentStorageBackend, StorageBackendResult,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Completed {
    Pruned(DiskANNPruneResult),
    Rebuilt,
}

enum Attempt {
    Ready,
    Commit(Completed),
    Read(DiskANNPruneResult),
    Rollback,
    Finished,
}

pub(super) struct Job {
    backend: Arc<dyn PersistentStorageBackend>,
    pruner: Option<Box<dyn DiskANNJournalPruner>>,
    capture: Option<Capture>,
    after: Option<DiskANNPruneCursor>,
    attempt: Attempt,
    completed: Option<Completed>,
    census: Option<DiskANNMaintenanceCensus>,
}

impl Job {
    pub(super) fn new(
        backend: Arc<dyn PersistentStorageBackend>,
        pruner: Box<dyn DiskANNJournalPruner>,
    ) -> Self {
        Self {
            backend,
            pruner: Some(pruner),
            capture: None,
            after: None,
            attempt: Attempt::Ready,
            completed: None,
            census: None,
        }
    }

    pub(super) fn with_rebuild(
        backend: Arc<dyn PersistentStorageBackend>,
        source: Box<dyn DiskANNMaintenanceSource>,
        options: DiskANNIndexOptions,
        configuration: Configuration,
    ) -> Self {
        Self {
            backend,
            pruner: None,
            capture: Some(Capture::new(source, options, configuration)),
            after: None,
            attempt: Attempt::Ready,
            completed: None,
            census: None,
        }
    }

    pub(super) fn phase(&self) -> Option<DiskANNMaintenancePhase> {
        if self.finished() {
            return None;
        }
        Some(if self.pending() {
            DiskANNMaintenancePhase::Completing
        } else if let Some(capture) = &self.capture {
            if capture.ready {
                DiskANNMaintenancePhase::Rebuilding
            } else {
                DiskANNMaintenancePhase::Counting
            }
        } else {
            DiskANNMaintenancePhase::Pruning
        })
    }

    pub(super) fn pending(&self) -> bool {
        matches!(
            self.attempt,
            Attempt::Commit(_) | Attempt::Read(_) | Attempt::Rollback
        )
    }

    pub(super) fn finished(&self) -> bool {
        matches!(self.attempt, Attempt::Finished)
    }

    pub(super) fn take_completed(&mut self) -> Option<Completed> {
        self.completed.take()
    }

    pub(super) fn take_census(&mut self) -> Option<DiskANNMaintenanceCensus> {
        self.census.take()
    }

    pub(super) fn step(&mut self, control: &StorageReadControl) -> StorageBackendResult<()> {
        if matches!(self.attempt, Attempt::Ready) {
            if let Err(error) = control.check() {
                self.attempt = Attempt::Finished;
                return Err(error);
            }
            if self.capture.as_ref().is_some_and(|capture| !capture.ready) {
                let result = self.count(control);
                if result.is_err() {
                    self.attempt = Attempt::Finished;
                }
                return result;
            }
            if let Err(error) = self.backend.begin_transaction() {
                self.attempt = if self.backend.in_transaction() {
                    Attempt::Rollback
                } else {
                    Attempt::Finished
                };
                return Err(error);
            }
            match self.evaluate(control) {
                Ok(Completed::Pruned(result)) if result.removed == 0 => {
                    self.attempt = Attempt::Read(result);
                }
                Ok(result) => self.attempt = Attempt::Commit(result),
                Err(error) => {
                    self.attempt = Attempt::Rollback;
                    // Preserve the original operation failure while retaining any still-active cleanup attempt.
                    let _ = self.finish();
                    return Err(error);
                }
            }
        }
        self.finish()
    }

    fn count(&mut self, control: &StorageReadControl) -> StorageBackendResult<()> {
        let capture = self.capture.as_mut().expect("counting retains its capture");
        if let Some(census) = capture.step(control)? {
            self.census = Some(census);
            if capture.configuration.policy.admits(census.changes) {
                capture.ready = true;
            } else {
                self.pruner = Some(self.capture.take().expect("counted capture").source);
            }
        }
        Ok(())
    }

    fn evaluate(&mut self, control: &StorageReadControl) -> StorageBackendResult<Completed> {
        if let Some(capture) = self.capture.take() {
            capture
                .source
                .rebuild(capture.options, &capture.configuration.temporary, control)?;
            return Ok(Completed::Rebuilt);
        }
        self.pruner
            .as_ref()
            .expect("pruning retains its source")
            .prune(
                DiskANNPruneRequest {
                    after: self.after,
                    max_records: 64,
                },
                control,
            )
            .map(Completed::Pruned)
    }

    fn finish(&mut self) -> StorageBackendResult<()> {
        match self.attempt {
            Attempt::Read(page) => match self.backend.rollback_transaction() {
                Ok(()) => self.accept(Completed::Pruned(page)),
                Err(error) => {
                    if !self.backend.in_transaction() {
                        if matches!(
                            error.transaction_outcome(),
                            Some(TransactionOutcome::Aborted(_))
                        ) {
                            self.accept(Completed::Pruned(page));
                        } else {
                            self.attempt = Attempt::Finished;
                        }
                    }
                    return Err(error);
                }
            },
            Attempt::Commit(page) => match self.backend.commit_transaction() {
                Ok(()) => self.accept(page),
                Err(error) => {
                    match error.transaction_outcome() {
                        Some(TransactionOutcome::Committed(_))
                            if !self.backend.in_transaction() =>
                        {
                            self.accept(page);
                        }
                        Some(
                            TransactionOutcome::Indeterminate(_) | TransactionOutcome::Committed(_),
                        ) => {}
                        _ if self.backend.in_transaction() => self.attempt = Attempt::Rollback,
                        _ => self.attempt = Attempt::Finished,
                    }
                    return Err(error);
                }
            },
            Attempt::Rollback => {
                if let Err(error) = self.backend.rollback_transaction() {
                    if !self.backend.in_transaction() {
                        self.attempt = Attempt::Finished;
                    }
                    return Err(error);
                }
                self.attempt = Attempt::Finished;
            }
            Attempt::Ready | Attempt::Finished => {}
        }
        Ok(())
    }

    fn accept(&mut self, completed: Completed) {
        self.completed = Some(completed);
        self.attempt = Attempt::Finished;
        if let Completed::Pruned(page) = completed {
            self.after = page.next;
            if page.next.is_some() {
                self.attempt = Attempt::Ready;
            }
        }
    }
}
