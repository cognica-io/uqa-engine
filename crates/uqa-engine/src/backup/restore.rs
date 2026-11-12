//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Restore admission and publication retain the candidate independently of its authorizing session.

use super::{
    memory::restore::database::{Input, MemoryState},
    start_error,
};
use crate::{Engine, SQLError};
use std::{mem, ops::Deref};
use uqa_analysis::AnalyzerResources;
use uqa_core::memory::{Budgeted, MemoryBudget, RetainedValue};
use uqa_storage::{
    backup::{
        BackupError, BackupId, BackupResult, BackupRoot, ImageProvider, LocalRepository,
        MemoryImageImportReader,
    },
    diskann_index::build::DiskANNTemporaryBudget,
    read_control::StorageReadControl,
};

/// The new database's storage owner. It never replaces an existing Engine.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub enum RestoreDestination {
    Memory,
}

#[derive(Clone, Copy, Debug)]
pub struct RestoreOptions {
    pub maximum_workspace_bytes: u64,
    pub maximum_value_bytes: usize,
    pub retained_bytes: usize,
}

impl RestoreOptions {
    pub fn new(maximum_workspace_bytes: u64, maximum_value_bytes: usize) -> Self {
        Self {
            maximum_workspace_bytes,
            maximum_value_bytes,
            retained_bytes: uqa_storage::mvcc::VersionedSessionOptions::default().retained_bytes,
        }
    }
}

/// Input and validation keep the candidate private until explicit publication.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RestorePhase {
    Reading,
    Validating,
    Prepared,
    Complete,
    Cancelled,
    Failed,
}

/// Clones share one restored runtime and preserve subsequent writes.
#[derive(Clone)]
pub struct RestoredEngine(RetainedValue<Engine>);

impl Deref for RestoredEngine {
    type Target = Engine;

    fn deref(&self) -> &Engine {
        &self.0
    }
}

enum State<'a> {
    Reading(Budgeted<Box<Input<'a>>>),
    Validating(Budgeted<Box<MemoryState>>),
    Prepared(RestoredEngine),
    Complete(RestoredEngine),
    Cancelled,
    Failed,
}

pub struct RestoreJob<'a> {
    state: State<'a>,
    backup: BackupId,
    root: BackupRoot,
    retained: MemoryBudget,
    control: StorageReadControl,
}

impl Engine {
    /// Authorize a fresh restoration through the ordinary transaction boundary. The job retains its controls and archive lease after the authorizing Engine closes.
    pub fn begin_restore<'a>(
        &self,
        repository: &'a LocalRepository,
        backup: BackupId,
        destination: RestoreDestination,
        options: RestoreOptions,
        control: &StorageReadControl,
    ) -> Result<RestoreJob<'a>, SQLError> {
        self.with_direct_query_snapshot(
            true,
            |engine| {
                control.check().map_err(|error| start_error(error.into()))?;
                if !engine.current_user_is_superuser() {
                    return Err(SQLError::Routine {
                        sqlstate: "42501".into(),
                        message: "database restore requires superuser authority".into(),
                    });
                }
                if options.maximum_workspace_bytes == 0
                    || options.maximum_value_bytes == 0
                    || options.retained_bytes == 0
                {
                    return Err(start_error(BackupError::Invalid("zero restore job limit")));
                }
                let control = control.with_temporary_limit(options.maximum_workspace_bytes);
                let lease = repository
                    .open_backup(backup, &control)
                    .map_err(start_error)?;
                let root = lease.root();
                let RestoreDestination::Memory = destination;
                if root.cut().format.provider() != ImageProvider::Memory {
                    return Err(start_error(BackupError::Unsupported(
                        "restore destination mapping",
                    )));
                }
                let memory = control
                    .memory()
                    .reserve(size_of::<Input<'_>>())
                    .map_err(|error| start_error(error.into()))?;
                let input = Input::new(
                    MemoryImageImportReader::new(lease, options.maximum_workspace_bytes, &control)
                        .map_err(start_error)?,
                    AnalyzerResources::default(),
                    DiskANNTemporaryBudget::new(options.maximum_workspace_bytes),
                    options.maximum_value_bytes,
                    &control,
                )
                .map_err(start_error)?;
                Ok(RestoreJob {
                    state: State::Reading(Budgeted::new(Box::new(input), memory)),
                    backup,
                    root,
                    retained: MemoryBudget::new(options.retained_bytes),
                    control,
                })
            },
            std::convert::identity,
        )
    }
}

impl RestoreJob<'_> {
    pub const fn backup(&self) -> BackupId {
        self.backup
    }

    pub const fn root(&self) -> BackupRoot {
        self.root
    }

    pub fn phase(&self) -> RestorePhase {
        match self.state {
            State::Reading(_) => RestorePhase::Reading,
            State::Validating(_) => RestorePhase::Validating,
            State::Prepared(_) => RestorePhase::Prepared,
            State::Complete(_) => RestorePhase::Complete,
            State::Cancelled => RestorePhase::Cancelled,
            State::Failed => RestorePhase::Failed,
        }
    }

    /// Advance repository verification or native input by its byte/poll window. The assembled-catalog validation boundary remains a whole operation.
    pub fn advance(&mut self, maximum_object_bytes: usize) -> BackupResult<RestorePhase> {
        match self.phase() {
            RestorePhase::Prepared | RestorePhase::Complete => return Ok(self.phase()),
            RestorePhase::Cancelled | RestorePhase::Failed => return Err(BackupError::Finished),
            _ => (),
        }
        let state = mem::replace(&mut self.state, State::Failed);
        self.control.check()?;
        if maximum_object_bytes == 0 {
            return Err(BackupError::Invalid("zero restore verification window"));
        }
        self.state = match state {
            State::Reading(mut input) => match input.as_mut().advance(maximum_object_bytes)? {
                Some(candidate) => {
                    drop(input);
                    let memory = self.control.memory().reserve(size_of::<MemoryState>())?;
                    State::Validating(Budgeted::new(Box::new(candidate), memory))
                }
                None => State::Reading(input),
            },
            State::Validating(candidate) => {
                let (candidate, memory) = candidate.into_parts();
                let engine = candidate.into_private_engine(self.retained.clone(), &self.control)?;
                drop(memory);
                State::Prepared(RestoredEngine(RetainedValue::with_budget(
                    engine,
                    self.control.memory(),
                )?))
            }
            _ => unreachable!("admitted active restore state"),
        };
        Ok(self.phase())
    }

    /// Publish once; repeated calls return the same runtime without replaying input or losing later writes.
    pub fn publish(&mut self) -> BackupResult<RestoredEngine> {
        if let State::Complete(engine) = &self.state {
            return Ok(engine.clone());
        }
        if self.phase() != RestorePhase::Prepared {
            return Err(BackupError::Finished);
        }
        let State::Prepared(engine) = mem::replace(&mut self.state, State::Failed) else {
            unreachable!("admitted prepared restore state")
        };
        self.control.check()?;
        self.state = State::Complete(engine.clone());
        Ok(engine)
    }

    /// Retire unpublished state. Completed results remain authoritative after late cancellation.
    pub fn cancel(&mut self) -> BackupResult<Option<RestoredEngine>> {
        if let State::Complete(engine) = &self.state {
            return Ok(Some(engine.clone()));
        }
        if !matches!(self.state, State::Failed | State::Cancelled) {
            self.state = State::Cancelled;
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests;
