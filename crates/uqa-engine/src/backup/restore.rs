//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Restore admission and publication own the candidate lifetime independently of the authorizing session.

use super::{
    memory::restore::database::{Input, MemoryState},
    start_error,
    validation::CatalogValidation,
};
use crate::{Engine, SQLError};
use std::{fmt, mem, ops::Deref, path::PathBuf, sync::Arc};
use uqa_core::image_io::ImageDecodeWindow;
use uqa_core::memory::{Budgeted, MemoryBudget, RetainedValue};
use uqa_storage::{
    backup::{
        BackupError, BackupId, BackupResult, BackupRoot, ImageProvider, LocalRepository,
        MemoryImageImportReader, PersistentBackupTarget, RestoreId,
    },
    diskann_index::build::DiskANNTemporaryBudget,
    read_control::StorageReadControl,
};

mod admission;
pub(in crate::backup) use admission::{AuthorizedRestore, RestoreRequest};
mod configuration;
mod functions;
pub(crate) use configuration::RestoreBindings;
pub use configuration::RestoreConfiguration;
use functions::Admission;

pub(super) fn input_window(maximum_object_bytes: usize) -> ImageDecodeWindow {
    ImageDecodeWindow {
        bytes: maximum_object_bytes,
        steps: maximum_object_bytes.min(64),
    }
}

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

impl RestoreDestination {
    /// Restore into a new directory using the supplied provider and file settings.
    pub fn persistent(
        directory: impl Into<PathBuf>,
        target: impl PersistentBackupTarget + 'static,
    ) -> Self {
        Self::Persistent {
            directory: directory.into(),
            target: Arc::new(target),
        }
    }
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

impl Default for RestoreOptions {
    fn default() -> Self {
        Self::new(super::DEFAULT_BACKUP_WORKSPACE_BYTES, 64 << 20)
    }
}

/// Reading and validation retain an unpublished candidate. Publication is explicit and remains stable after a lost reply or late cancellation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RestorePhase {
    Reading,
    Validating,
    Prepared,
    /// The job is rechecking a recovered prepared image or opening the installed runtime.
    Publishing,
    Complete,
    Cancelled,
    Failed,
}

impl RestorePhase {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Reading => "reading",
            Self::Validating => "validating",
            Self::Prepared => "prepared",
            Self::Publishing => "publishing",
            Self::Complete => "complete",
            Self::Cancelled => "cancelled",
            Self::Failed => "failed",
        }
    }
}

/// An independently retained restored runtime. Clones refer to the same published Engine and preserve later writes.
#[derive(Clone)]
pub struct RestoredEngine(RetainedValue<Engine>);

impl RestoredEngine {
    /// Transfer the published session and its memory reservation to a host Engine handle.
    pub fn into_retained(self) -> RetainedValue<Engine> {
        self.0
    }
}

impl Deref for RestoredEngine {
    type Target = Engine;
    fn deref(&self) -> &Engine {
        &self.0
    }
}

enum State<'a> {
    Reading(
        Budgeted<Box<Input<'a>>>,
        RestoreBindings,
        Option<Admission<'a>>,
    ),
    Decoded(Budgeted<Box<MemoryState>>, RestoreBindings),
    Validating(RestoredEngine, CatalogValidation, RestoreBindings),
    Persistent(Budgeted<Box<persistent::Persistent<'a>>>),
    Prepared(RestoredEngine, RestoreBindings),
    Opening(persistent::PublishedInput),
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
        options: impl Into<RestoreConfiguration>,
        control: &StorageReadControl,
    ) -> Result<RestoreJob<'a>, SQLError> {
        RestoreRequest::Begin(backup).admit(self, destination, options, control, |request| {
            request.open(repository)
        })
    }

    /// Resolve a durable persistent restore through the same authority and transaction boundary. Prepared and completed requests do not require the source archive; completion remains authoritative after cancellation.
    pub fn resume_restore<'a>(
        &self,
        repository: &'a LocalRepository,
        id: RestoreId,
        destination: RestoreDestination,
        options: impl Into<RestoreConfiguration>,
        control: &StorageReadControl,
    ) -> Result<RestoreJob<'a>, SQLError> {
        RestoreRequest::Resume(id).admit(self, destination, options, control, |request| {
            request.open(repository)
        })
    }

    /// Cancel an interrupted persistent request by its durable identity, including after a failed job has released its files. A previously published request returns its current runtime without replacing later writes.
    pub fn cancel_restore(
        &self,
        repository: &LocalRepository,
        id: RestoreId,
        destination: RestoreDestination,
        options: impl Into<RestoreConfiguration>,
        control: &StorageReadControl,
    ) -> Result<Option<RestoredEngine>, SQLError> {
        self.with_direct_query_snapshot(
            true,
            |engine| {
                let configuration = options.into();
                engine.authorize_restore(configuration.options)?;
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
                    &configuration.admit(),
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
            State::Reading(..) => RestorePhase::Reading,
            State::Decoded(..) | State::Validating(..) => RestorePhase::Validating,
            State::Persistent(ref candidate) => candidate.phase(),
            State::Prepared(..) => RestorePhase::Prepared,
            State::Opening(_) => RestorePhase::Publishing,
            State::Complete(_) => RestorePhase::Complete,
            State::Cancelled => RestorePhase::Cancelled,
            State::Failed => RestorePhase::Failed,
        }
    }

    /// Advance repository input by at most `maximum_object_bytes`, or run up to `min(maximum_object_bytes, 64)` catalog steps. A persistent candidate retains its private read transaction until validation completes.
    pub fn advance(&mut self, maximum_object_bytes: usize) -> BackupResult<RestorePhase> {
        match self.phase() {
            RestorePhase::Prepared | RestorePhase::Complete => return Ok(self.phase()),
            RestorePhase::Publishing => {
                self.advance_publication(input_window(maximum_object_bytes).steps)?;
                return Ok(self.phase());
            }
            RestorePhase::Cancelled | RestorePhase::Failed => return Err(BackupError::Finished),
            _ => (),
        }
        let state = mem::replace(&mut self.state, State::Failed);
        self.control.check()?;
        if maximum_object_bytes == 0 {
            return Err(BackupError::Invalid("zero restore verification window"));
        }
        self.state = match state {
            State::Reading(mut input, bindings, mut functions) => {
                if !Admission::advance(
                    &mut functions,
                    maximum_object_bytes,
                    &bindings,
                    &self.control,
                )? {
                    self.state = State::Reading(input, bindings, functions);
                    return Ok(self.phase());
                }
                match input.as_mut().advance(maximum_object_bytes)? {
                    Some(candidate) => {
                        drop(input);
                        let memory = self.control.memory().reserve(size_of::<MemoryState>())?;
                        State::Decoded(Budgeted::new(Box::new(candidate), memory), bindings)
                    }
                    None => State::Reading(input, bindings, functions),
                }
            }
            State::Decoded(candidate, bindings) => {
                let (candidate, memory) = candidate.into_parts();
                let mut engine = candidate.assemble(self.retained.clone(), &self.control)?;
                bindings.attach(&mut engine);
                drop(memory);
                let engine =
                    RestoredEngine(RetainedValue::with_budget(engine, self.control.memory())?);
                let checks = CatalogValidation::begin(&engine, &self.control)?;
                State::Validating(engine, checks, bindings)
            }
            State::Validating(engine, checks, bindings) => {
                match checks.advance(
                    &engine,
                    |restored| restored,
                    &self.control,
                    input_window(maximum_object_bytes).steps,
                )? {
                    Some(checks) => State::Validating(engine, checks, bindings),
                    None => State::Prepared(engine, bindings),
                }
            }
            State::Persistent(mut candidate) => {
                candidate.as_mut().advance(maximum_object_bytes)?;
                State::Persistent(candidate)
            }
            _ => unreachable!("admitted active restore state"),
        };
        Ok(self.phase())
    }

    /// Explicitly publish a prepared restore and open its writable runtime. Each call performs a publication transition or advances at most `maximum_steps` catalog steps. Filesystem synchronization may block. After durable installation the request remains recoverable even if activation is interrupted.
    pub fn advance_publication(
        &mut self,
        maximum_steps: usize,
    ) -> BackupResult<Option<RestoredEngine>> {
        if let State::Complete(engine) = &self.state {
            return Ok(Some(engine.clone()));
        }
        if !matches!(
            self.phase(),
            RestorePhase::Prepared | RestorePhase::Publishing
        ) {
            return Err(BackupError::Finished);
        }
        if maximum_steps == 0 {
            return Err(BackupError::Invalid("zero restore publication step limit"));
        }
        self.state = match mem::replace(&mut self.state, State::Failed) {
            State::Prepared(engine, _functions) => {
                self.control.check()?;
                State::Complete(engine)
            }
            State::Persistent(mut candidate) => {
                match candidate.as_mut().advance_publication(
                    maximum_steps,
                    self.retained.clone(),
                    &self.control,
                )? {
                    Some(input) => State::Opening(input),
                    None => State::Persistent(candidate),
                }
            }
            State::Opening(mut input) => match input.advance(maximum_steps)? {
                Some(engine) => State::Complete(engine),
                None => State::Opening(input),
            },
            _ => unreachable!("admitted restore publication"),
        };
        match &self.state {
            State::Complete(engine) => Ok(Some(engine.clone())),
            _ => Ok(None),
        }
    }

    /// Publish and open the runtime to completion. Repeated publication returns the same Engine and preserves later writes.
    pub fn publish(&mut self) -> BackupResult<RestoredEngine> {
        loop {
            if let Some(engine) = self.advance_publication(64)? {
                return Ok(engine);
            }
        }
    }

    /// Drive a fresh or resumed restoration through verification and publication.
    pub fn finish(&mut self) -> BackupResult<RestoredEngine> {
        loop {
            match self.advance(16 * 1024)? {
                RestorePhase::Prepared | RestorePhase::Publishing | RestorePhase::Complete => {
                    return self.publish();
                }
                _ => (),
            }
        }
    }

    /// Retire an unpublished candidate. A completed result remains authoritative after late cancellation. A failed persistent job has already closed its files; its durable identity remains available to `Engine::resume_restore` and `Engine::cancel_restore`.
    pub fn cancel(&mut self) -> BackupResult<Option<RestoredEngine>> {
        if let State::Complete(engine) = &self.state {
            return Ok(Some(engine.clone()));
        }
        if matches!(self.state, State::Opening(_)) {
            return self.publish().map(Some);
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
