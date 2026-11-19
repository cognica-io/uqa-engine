//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Restore admission and publication own the candidate lifetime independently of the authorizing session.

use super::{
    memory::restore::database::{Input, MemoryState},
    start_error,
};
use crate::{Engine, SQLError};
use std::{fmt, mem, ops::Deref, path::PathBuf, sync::Arc};
use uqa_analysis::AnalyzerResources;
use uqa_core::memory::{Budgeted, MemoryBudget, RetainedValue};
use uqa_storage::{
    backup::{
        BackupError, BackupId, BackupResult, BackupRoot, ImageProvider, LocalRepository,
        MemoryImageImportReader, PersistentBackupTarget, RestoreId,
    },
    diskann_index::build::DiskANNTemporaryBudget,
    read_control::StorageReadControl,
};

/// The new database's storage owner. It never replaces an existing Engine.
#[derive(Clone)]
#[non_exhaustive]
pub enum RestoreDestination {
    Memory,
    Persistent {
        directory: PathBuf,
        target: Arc<dyn PersistentBackupTarget>,
    },
}

impl fmt::Debug for RestoreDestination {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Memory => formatter.write_str("Memory"),
            Self::Persistent { directory, target } => formatter
                .debug_struct("Persistent")
                .field("directory", directory)
                .field("provider", &target.image_provider())
                .finish_non_exhaustive(),
        }
    }
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

/// Reading and validation retain an unpublished candidate. Publication is explicit and remains stable after a lost reply or late cancellation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RestorePhase {
    Reading,
    Validating,
    Prepared,
    Complete,
    Cancelled,
    Failed,
}

/// An independently retained restored runtime. Clones refer to the same published Engine and preserve later writes.
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
    Persistent(Budgeted<Box<persistent::Persistent<'a>>>),
    Prepared(RestoredEngine),
    Complete(RestoredEngine),
    Cancelled,
    Failed,
}

pub struct RestoreJob<'a> {
    state: State<'a>,
    backup: BackupId,
    root: Option<BackupRoot>,
    id: Option<RestoreId>,
    retained: MemoryBudget,
    control: StorageReadControl,
}

impl Engine {
    /// Authorize a fresh restoration through the ordinary transaction boundary. The returned job retains its controls and archive lease; the authorizing Engine may be closed immediately.
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
                control.check().map_err(|e| start_error(e.into()))?;
                engine.authorize_restore(options)?;
                let control = control.with_temporary_limit(options.maximum_workspace_bytes);
                let lease = repository
                    .open_backup(backup, &control)
                    .map_err(start_error)?;
                let root = lease.root();
                match destination {
                    RestoreDestination::Memory => {
                        if root.cut().format.provider() != ImageProvider::Memory {
                            return Err(start_error(BackupError::Unsupported(
                                "restore destination mapping",
                            )));
                        }
                        let memory = control
                            .memory()
                            .reserve(size_of::<Input<'_>>())
                            .map_err(|e| start_error(e.into()))?;
                        let input = Input::new(
                            MemoryImageImportReader::new(
                                lease,
                                options.maximum_workspace_bytes,
                                &control,
                            )
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
                            root: Some(root),
                            id: None,
                            retained: MemoryBudget::new(options.retained_bytes),
                            control,
                        })
                    }
                    RestoreDestination::Persistent { directory, target } => {
                        if root.cut().format.provider() != target.image_provider() {
                            return Err(start_error(BackupError::Unsupported(
                                "restore destination mapping",
                            )));
                        }
                        drop(lease);
                        let memory = control
                            .memory()
                            .reserve(size_of::<persistent::Persistent<'_>>())
                            .map_err(|e| start_error(e.into()))?;
                        let candidate = persistent::Persistent::begin(
                            repository, backup, directory, target, options, &control,
                        )
                        .map_err(start_error)?;
                        let id = candidate.id();
                        Ok(RestoreJob {
                            state: State::Persistent(Budgeted::new(Box::new(candidate), memory)),
                            backup,
                            root: Some(root),
                            id: Some(id),
                            retained: MemoryBudget::new(options.retained_bytes),
                            control,
                        })
                    }
                }
            },
            std::convert::identity,
        )
    }

    /// Resolve a durable persistent restore through the same authority and transaction boundary. Prepared and completed requests do not require the source archive; completion remains authoritative after cancellation.
    pub fn resume_restore<'a>(
        &self,
        repository: &'a LocalRepository,
        id: RestoreId,
        destination: RestoreDestination,
        options: RestoreOptions,
        control: &StorageReadControl,
    ) -> Result<RestoreJob<'a>, SQLError> {
        self.with_direct_query_snapshot(
            true,
            |engine| {
                engine.authorize_restore(options)?;
                let RestoreDestination::Persistent { directory, target } = destination else {
                    return Err(start_error(BackupError::Unsupported(
                        "durable memory restore recovery",
                    )));
                };
                let control = control.with_temporary_limit(options.maximum_workspace_bytes);
                let memory = control
                    .memory()
                    .reserve(size_of::<persistent::Persistent<'_>>())
                    .map_err(|e| start_error(e.into()))?;
                let (candidate, backup) = persistent::Persistent::resume(
                    repository, id, directory, target, options, &control,
                )
                .map_err(start_error)?;
                Ok(RestoreJob {
                    state: State::Persistent(Budgeted::new(Box::new(candidate), memory)),
                    backup,
                    root: None,
                    id: Some(id),
                    retained: MemoryBudget::new(options.retained_bytes),
                    control,
                })
            },
            std::convert::identity,
        )
    }

    /// Cancel an interrupted persistent request by its durable identity, including after a failed job has released its files. A previously published request returns its current runtime without replacing later writes.
    pub fn cancel_restore(
        &self,
        repository: &LocalRepository,
        id: RestoreId,
        destination: RestoreDestination,
        options: RestoreOptions,
        control: &StorageReadControl,
    ) -> Result<Option<RestoredEngine>, SQLError> {
        self.with_direct_query_snapshot(
            true,
            |engine| {
                engine.authorize_restore(options)?;
                let RestoreDestination::Persistent { directory, target } = destination else {
                    return Err(start_error(BackupError::Unsupported(
                        "durable memory restore recovery",
                    )));
                };
                persistent::cancel(
                    repository,
                    id,
                    directory.as_path(),
                    target.as_ref(),
                    options,
                    control,
                )
                .map_err(start_error)
            },
            std::convert::identity,
        )
    }

    fn authorize_restore(&self, options: RestoreOptions) -> Result<(), SQLError> {
        if !self.current_user_is_superuser() {
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
        Ok(())
    }
}

impl RestoreJob<'_> {
    pub const fn backup(&self) -> BackupId {
        self.backup
    }

    /// The root read at fresh admission. Durable recovery may succeed after the archive is gone.
    pub const fn root(&self) -> Option<BackupRoot> {
        self.root
    }

    /// Retain this identity to resolve a persistent restore after interruption or a lost completion reply.
    pub const fn id(&self) -> Option<RestoreId> {
        self.id
    }

    pub fn phase(&self) -> RestorePhase {
        match self.state {
            State::Reading(_) => RestorePhase::Reading,
            State::Validating(_) => RestorePhase::Validating,
            State::Persistent(ref candidate) => candidate.phase(),
            State::Prepared(_) => RestorePhase::Prepared,
            State::Complete(_) => RestorePhase::Complete,
            State::Cancelled => RestorePhase::Cancelled,
            State::Failed => RestorePhase::Failed,
        }
    }

    /// Advance repository verification and native input under their byte/poll windows. Persistent tables retain one read snapshot across calls. Remaining registry loading, assembled validation and published-runtime activation are whole operations.
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
            State::Persistent(mut candidate) => {
                candidate.as_mut().advance(maximum_object_bytes)?;
                State::Persistent(candidate)
            }
            _ => unreachable!("admitted active restore state"),
        };
        Ok(self.phase())
    }

    /// Publish the prepared owner once; repeated publication returns the same runtime without replaying input or losing later writes.
    pub fn publish(&mut self) -> BackupResult<RestoredEngine> {
        if let State::Complete(engine) = &self.state {
            return Ok(engine.clone());
        }
        if self.phase() != RestorePhase::Prepared {
            return Err(BackupError::Finished);
        }
        let engine = match mem::replace(&mut self.state, State::Failed) {
            State::Prepared(engine) => {
                self.control.check()?;
                engine
            }
            State::Persistent(mut candidate) => {
                let engine = candidate
                    .as_mut()
                    .publish(self.retained.clone(), &self.control)?;
                drop(candidate);
                engine
            }
            _ => unreachable!(),
        };
        self.state = State::Complete(engine.clone());
        Ok(engine)
    }

    /// Retire an unpublished candidate. A completed result remains authoritative after late cancellation. A failed persistent job has already closed its files; its durable identity remains available to `Engine::resume_restore` and `Engine::cancel_restore`.
    pub fn cancel(&mut self) -> BackupResult<Option<RestoredEngine>> {
        if let State::Complete(engine) = &self.state {
            return Ok(Some(engine.clone()));
        }
        if matches!(self.state, State::Failed | State::Cancelled) {
            return Ok(None);
        }
        if let State::Persistent(mut candidate) = mem::replace(&mut self.state, State::Failed) {
            let result = candidate
                .as_mut()
                .cancel(self.retained.clone(), &self.control)?;
            drop(candidate);
            if let Some(engine) = result {
                self.state = State::Complete(engine.clone());
                return Ok(Some(engine));
            }
        }
        self.state = State::Cancelled;
        Ok(None)
    }
}

mod persistent;

#[cfg(test)]
mod tests;
