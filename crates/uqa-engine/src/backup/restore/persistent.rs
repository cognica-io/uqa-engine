//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Storage owns durable admission and publication; Engine supplies immutable catalog validation and fresh runtime activation.

use super::{RestoreOptions, RestorePhase, RestoredEngine};
use crate::Engine;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use uqa_core::memory::{Budgeted, MemoryBudget, RetainedValue};
use uqa_storage::{
    backup::{
        BackupId, BackupImageImportStep, BackupImageValidationStep, BackupImageValidationWork,
        BackupImageValidator, BackupImportLimits, BackupLease, BackupRestoreFactory,
        BackupRestoreJob, BackupRestoreOperation, BackupRestoreStep, BackupRestoreWork,
        BackupRestoreWorkStep, BackupResult, LocalRepository, OwnedBackupImageValidation,
        OwnedPersistentBackupImport, PersistentBackupTarget, RestoreDirectory, RestoreId,
    },
    catalog::input::CatalogInputLimits,
    mvcc::DatabaseRestore,
    read_control::{CancellationToken, StorageReadControl},
    PersistentStorageProvider,
};

pub(super) struct Persistent<'a> {
    execution: Execution<'a>,
    target: Arc<dyn PersistentBackupTarget>,
    directory: Budgeted<PathBuf>,
    options: RestoreOptions,
    request: DatabaseRestore,
    id: RestoreId,
}

enum Execution<'a> {
    Admitted(BackupRestoreOperation<'a>),
    Active(BackupRestoreJob<'a, Work<'a>>),
    Failed,
}

impl<'a> Persistent<'a> {
    pub(super) fn begin(
        repository: &'a LocalRepository,
        backup: BackupId,
        directory: PathBuf,
        target: Arc<dyn PersistentBackupTarget>,
        options: RestoreOptions,
        control: &StorageReadControl,
    ) -> BackupResult<Self> {
        let memory = control.memory().reserve(directory.capacity())?;
        let directory = Budgeted::new(directory, memory);
        let operation = repository.begin_restore(backup, directory.as_path(), control)?;
        Ok(Self::from_operation(operation, directory, target, options))
    }

    pub(super) fn resume(
        repository: &'a LocalRepository,
        id: RestoreId,
        directory: PathBuf,
        target: Arc<dyn PersistentBackupTarget>,
        options: RestoreOptions,
        control: &StorageReadControl,
    ) -> BackupResult<(Self, BackupId)> {
        let memory = control.memory().reserve(directory.capacity())?;
        let directory = Budgeted::new(directory, memory);
        let operation = repository.resume_restore(id, directory.as_path(), control)?;
        let backup = operation.backup();
        Ok((
            Self::from_operation(operation, directory, target, options),
            backup,
        ))
    }

    fn from_operation(
        operation: BackupRestoreOperation<'a>,
        directory: Budgeted<PathBuf>,
        target: Arc<dyn PersistentBackupTarget>,
        options: RestoreOptions,
    ) -> Self {
        Self {
            request: operation.request(),
            id: operation.id(),
            execution: Execution::Admitted(operation),
            target,
            directory,
            options,
        }
    }

    pub(super) fn id(&self) -> RestoreId {
        self.id
    }

    pub(super) fn phase(&self) -> RestorePhase {
        let job = match &self.execution {
            Execution::Admitted(operation) => {
                return match operation.progress() {
                    Ok(BackupRestoreStep::Pending) => RestorePhase::Reading,
                    Ok(BackupRestoreStep::Prepared | BackupRestoreStep::Complete) => {
                        RestorePhase::Prepared
                    }
                    Err(_) => RestorePhase::Cancelled,
                }
            }
            Execution::Active(job) => job,
            Execution::Failed => return RestorePhase::Failed,
        };
        match job.work() {
            Some(Work {
                stage: Stage::Reading(_),
                ..
            }) => RestorePhase::Reading,
            Some(_) => RestorePhase::Validating,
            None => RestorePhase::Prepared,
        }
    }

    pub(super) fn advance(&mut self, maximum_object_bytes: usize) -> BackupResult<RestorePhase> {
        let execution = std::mem::replace(&mut self.execution, Execution::Failed);
        let job = match execution {
            Execution::Admitted(operation) => operation.into_job(Factory {
                target: Arc::clone(&self.target),
                options: self.options,
            })?,
            Execution::Active(mut job) => {
                job.advance_with(|work| work.advance(maximum_object_bytes))?;
                job
            }
            Execution::Failed => return Err(uqa_storage::backup::BackupError::Finished),
        };
        self.execution = Execution::Active(job);
        Ok(self.phase())
    }

    fn durable_publish(&mut self) -> BackupResult<()> {
        match &mut self.execution {
            Execution::Admitted(operation) => {
                operation.publish()?;
            }
            Execution::Active(job) => {
                job.publish()?;
            }
            Execution::Failed => return Err(uqa_storage::backup::BackupError::Finished),
        }
        Ok(())
    }

    pub(super) fn publish(
        &mut self,
        retained: MemoryBudget,
        control: &StorageReadControl,
    ) -> BackupResult<RestoredEngine> {
        self.durable_publish()?;
        self.open_published(retained, control)
    }

    pub(super) fn cancel(
        &mut self,
        retained: MemoryBudget,
        control: &StorageReadControl,
    ) -> BackupResult<Option<RestoredEngine>> {
        let completed = match &mut self.execution {
            Execution::Admitted(operation) => operation.cancel()?,
            Execution::Active(job) => job.cancel()?,
            Execution::Failed => return Err(uqa_storage::backup::BackupError::Finished),
        };
        if completed.is_some() {
            self.open_published(retained, control).map(Some)
        } else {
            Ok(None)
        }
    }

    fn open_published(
        &self,
        retained: MemoryBudget,
        control: &StorageReadControl,
    ) -> BackupResult<RestoredEngine> {
        open_published(
            self.target.as_ref(),
            self.directory.as_path(),
            self.request,
            self.options,
            retained,
            control,
        )
    }
}

pub(super) fn cancel(
    repository: &LocalRepository,
    id: RestoreId,
    directory: &Path,
    target: &dyn PersistentBackupTarget,
    options: RestoreOptions,
    control: &StorageReadControl,
) -> BackupResult<Option<RestoredEngine>> {
    let operation = repository.cancel_restore(id, directory, control)?;
    let request = operation.request();
    if operation.completed().is_none() {
        return Ok(None);
    }
    open_published(
        target,
        directory,
        request,
        options,
        MemoryBudget::new(options.retained_bytes),
        control,
    )
    .map(Some)
}

fn open_published(
    target: &dyn PersistentBackupTarget,
    directory: &std::path::Path,
    request: DatabaseRestore,
    options: RestoreOptions,
    retained: MemoryBudget,
    control: &StorageReadControl,
) -> BackupResult<RestoredEngine> {
    // A durable completion remains authoritative after cancellation or a lost reply. Opening resolves that exact request and never recreates a missing destination or replays history.
    let completion = StorageReadControl::new(control.memory(), &CancellationToken::new());
    let provider = target.open(directory, request, &completion)?;
    let engine = Engine::from_restored_persistent_provider(
        &provider,
        retained,
        catalog_limits(options),
        &completion,
    )?;
    Ok(RestoredEngine(RetainedValue::with_budget(
        engine,
        control.memory(),
    )?))
}

struct Factory {
    target: Arc<dyn PersistentBackupTarget>,
    options: RestoreOptions,
}

impl<'a> BackupRestoreFactory<'a> for Factory {
    type Work = Work<'a>;

    fn import(
        self,
        directory: &RestoreDirectory<'_>,
        backup: BackupLease<'a>,
        request: DatabaseRestore,
        control: &StorageReadControl,
    ) -> BackupResult<Self::Work> {
        let input = self
            .target
            .import(directory, backup, request, self.limits(), control)?;
        Ok(self.work(input, control))
    }

    fn recover(
        self,
        directory: &RestoreDirectory<'_>,
        backup: BackupLease<'a>,
        request: DatabaseRestore,
        control: &StorageReadControl,
    ) -> BackupResult<Self::Work> {
        let input = self
            .target
            .recover(directory, backup, request, self.limits(), control)?;
        Ok(self.work(input, control))
    }
}

impl Factory {
    fn limits(&self) -> BackupImportLimits {
        BackupImportLimits {
            maximum_workspace_bytes: self.options.maximum_workspace_bytes,
            maximum_value_bytes: self.options.maximum_value_bytes,
        }
    }

    fn work<'a>(
        &self,
        input: OwnedPersistentBackupImport<'a>,
        control: &StorageReadControl,
    ) -> Work<'a> {
        Work {
            stage: Stage::Reading(input),
            retention: MemoryBudget::new(self.options.retained_bytes),
            limits: catalog_limits(self.options),
            control: control.clone(),
        }
    }
}

enum Stage<'a> {
    Reading(OwnedPersistentBackupImport<'a>),
    Validating(OwnedPersistentBackupImport<'a>),
    Complete,
    Failed,
}

pub(super) struct Work<'a> {
    stage: Stage<'a>,
    retention: MemoryBudget,
    limits: CatalogInputLimits,
    control: StorageReadControl,
}

impl BackupRestoreWork for Work<'_> {
    fn advance(&mut self) -> BackupResult<BackupRestoreWorkStep> {
        self.advance(16 * 1024)
    }
}

impl Work<'_> {
    fn advance(&mut self, maximum_object_bytes: usize) -> BackupResult<BackupRestoreWorkStep> {
        if matches!(self.stage, Stage::Complete) {
            return Ok(BackupRestoreWorkStep::Complete);
        }
        let stage = std::mem::replace(&mut self.stage, Stage::Failed);
        self.control.check()?;
        self.stage = match stage {
            Stage::Reading(mut input) => match input.advance(maximum_object_bytes)? {
                BackupImageImportStep::Pending => Stage::Reading(input),
                BackupImageImportStep::Imported => Stage::Validating(input),
            },
            Stage::Validating(mut input) => {
                match input.advance_validation(
                    &mut Validator {
                        retention: self.retention.clone(),
                        limits: self.limits,
                    },
                    1,
                )? {
                    BackupImageValidationStep::Pending => Stage::Validating(input),
                    BackupImageValidationStep::Complete => Stage::Complete,
                }
            }
            Stage::Failed => return Err(uqa_storage::backup::BackupError::Finished),
            Stage::Complete => unreachable!(),
        };
        Ok(if matches!(self.stage, Stage::Complete) {
            BackupRestoreWorkStep::Complete
        } else {
            BackupRestoreWorkStep::Pending
        })
    }
}

fn catalog_limits(options: RestoreOptions) -> CatalogInputLimits {
    CatalogInputLimits {
        maximum_key_bytes: options.maximum_value_bytes,
        maximum_value_bytes: options.maximum_value_bytes,
    }
}

struct Validator {
    retention: MemoryBudget,
    limits: CatalogInputLimits,
}

impl BackupImageValidator for Validator {
    fn validate(
        &mut self,
        provider: Arc<dyn PersistentStorageProvider>,
        control: &StorageReadControl,
    ) -> BackupResult<()> {
        let mut work = self.begin(provider, control)?;
        while work.advance(64)? == BackupImageValidationStep::Pending {}
        Ok(())
    }

    fn begin(
        &mut self,
        provider: Arc<dyn PersistentStorageProvider>,
        control: &StorageReadControl,
    ) -> BackupResult<OwnedBackupImageValidation> {
        OwnedBackupImageValidation::create(control, || {
            Ok(Validation {
                input: crate::open::PersistentEngineInput::new(
                    provider,
                    self.retention.clone(),
                    self.limits,
                    control,
                )?,
                control: control.clone(),
            })
        })
    }
}

struct Validation {
    input: crate::open::PersistentEngineInput,
    control: StorageReadControl,
}

impl BackupImageValidationWork for Validation {
    fn advance(&mut self, maximum_steps: usize) -> BackupResult<BackupImageValidationStep> {
        let Some(session) = self.input.advance(maximum_steps)? else {
            return Ok(BackupImageValidationStep::Pending);
        };
        crate::backup::validation::validate(session.engine(), &self.control)?;
        drop(session.finish()?);
        Ok(BackupImageValidationStep::Complete)
    }
}
