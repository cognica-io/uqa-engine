//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Advance the journal cursor only after the original evaluated transaction completes.

use std::sync::Arc;
use uqa_storage::{
    diskann_index::changes::{
        DiskANNJournalPruner, DiskANNPruneCursor, DiskANNPruneRequest, DiskANNPruneResult,
    },
    mvcc::TransactionOutcome,
    read_control::StorageReadControl,
    PersistentStorageBackend, StorageBackendResult,
};

enum Attempt {
    Ready,
    Commit(DiskANNPruneResult),
    Read(DiskANNPruneResult),
    Rollback,
    Finished,
}

pub(super) struct Job {
    backend: Arc<dyn PersistentStorageBackend>,
    pruner: Box<dyn DiskANNJournalPruner>,
    after: Option<DiskANNPruneCursor>,
    attempt: Attempt,
    completed: Option<DiskANNPruneResult>,
}

impl Job {
    pub(super) fn new(
        backend: Arc<dyn PersistentStorageBackend>,
        pruner: Box<dyn DiskANNJournalPruner>,
    ) -> Self {
        Self {
            backend,
            pruner,
            after: None,
            attempt: Attempt::Ready,
            completed: None,
        }
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

    pub(super) fn take_completed(&mut self) -> Option<DiskANNPruneResult> {
        self.completed.take()
    }

    pub(super) fn step(&mut self, control: &StorageReadControl) -> StorageBackendResult<()> {
        if matches!(self.attempt, Attempt::Ready) {
            if let Err(error) = control.check() {
                self.attempt = Attempt::Finished;
                return Err(error);
            }
            if let Err(error) = self.backend.begin_transaction() {
                self.attempt = if self.backend.in_transaction() {
                    Attempt::Rollback
                } else {
                    Attempt::Finished
                };
                return Err(error);
            }
            match self.pruner.prune(
                DiskANNPruneRequest {
                    after: self.after,
                    max_records: 64,
                },
                control,
            ) {
                Ok(result) => {
                    self.attempt = if result.removed == 0 {
                        Attempt::Read(result)
                    } else {
                        Attempt::Commit(result)
                    }
                }
                Err(error) => {
                    self.attempt = Attempt::Rollback;
                    // Preserve a failed cleanup attempt for the next step; never start another transaction on this session.
                    self.finish()?;
                    return Err(error);
                }
            }
        }
        self.finish()
    }

    fn finish(&mut self) -> StorageBackendResult<()> {
        match self.attempt {
            Attempt::Read(page) => match self.backend.rollback_transaction() {
                Ok(()) => self.accept(page),
                Err(error) => {
                    if !self.backend.in_transaction() {
                        if matches!(
                            error.transaction_outcome(),
                            Some(TransactionOutcome::Aborted(_))
                        ) {
                            self.accept(page);
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
                self.backend.rollback_transaction()?;
                self.attempt = Attempt::Finished;
            }
            Attempt::Ready | Attempt::Finished => {}
        }
        Ok(())
    }

    fn accept(&mut self, page: DiskANNPruneResult) {
        self.after = page.next;
        self.completed = Some(page);
        self.attempt = if page.next.is_some() {
            Attempt::Ready
        } else {
            Attempt::Finished
        };
    }
}
