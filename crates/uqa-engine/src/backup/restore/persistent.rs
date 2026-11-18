//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Storage owns durable admission and publication; Engine supplies immutable catalog validation and fresh runtime activation.

use super::{
    configuration::AdmittedRestore, RestoreBindings, RestoreOptions, RestorePhase, RestoredEngine,
};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use uqa_core::memory::{Budgeted, MemoryBudget};
use uqa_storage::{
    backup::{
        BackupId, BackupImageImportStep, BackupImageValidationStep, BackupImageValidationWork,
        BackupImageValidator, BackupImportLimits, BackupLease, BackupRestoreFactory,
        BackupRestoreJob, BackupRestoreOperation, BackupRestoreStep, BackupRestoreWork,
        BackupRestoreWorkStep, BackupResult, LocalRepository, OwnedBackupImageValidation,
        OwnedPersistentBackupImport, PersistentBackupTarget, PreparedRestoreStep,
        PreparedRestoreValidation, RestoreDirectory, RestoreId,
    },
    catalog::input::CatalogInputLimits,
    mvcc::DatabaseRestore,
    read_control::StorageReadControl,
    PersistentStorageProvider,
};

mod activation;
pub(super) use activation::PublishedInput;

pub(super) struct Persistent<'a> {
    execution: Execution<'a>,
    target: Arc<dyn PersistentBackupTarget>,
    directory: Budgeted<PathBuf>,
    options: AdmittedRestore,
    request: DatabaseRestore,
    id: RestoreId,
}

enum Execution<'a> {
    Admitted(BackupRestoreOperation<'a>),
    Active(BackupRestoreJob<'a, Work<'a>>),
    CheckingPrepared(PreparedRestoreValidation<'a, OwnedBackupImageValidation>),
    Failed,
}

impl<'a> Persistent<'a> {
    pub(super) fn begin(
        repository: &'a LocalRepository,
        backup: BackupId,
        directory: PathBuf,
        target: Arc<dyn PersistentBackupTarget>,
        options: AdmittedRestore,
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
        options: AdmittedRestore,
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
        options: AdmittedRestore,
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
            Execution::CheckingPrepared(_) => return RestorePhase::Publishing,
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
                options: self.options.clone(),
            })?,
            Execution::Active(mut job) => {
                job.advance_with(|work| work.advance(maximum_object_bytes))?;
                job
            }
            Execution::CheckingPrepared(_) | Execution::Failed => {
                return Err(uqa_storage::backup::BackupError::Finished)
            }
        };
        self.execution = Execution::Active(job);
        Ok(self.phase())
    }

    pub(super) fn advance_publication(
        &mut self,
        maximum_steps: usize,
        retained: MemoryBudget,
        control: &StorageReadControl,
    ) -> BackupResult<Option<PublishedInput>> {
        let execution = std::mem::replace(&mut self.execution, Execution::Failed);
        let step = match execution {
            Execution::Admitted(operation) => {
                operation.begin_prepared_validation(|directory, request, original| {
                    let provider = self.target.open(directory, request, original)?;
                    Validator::new(&self.options).begin(provider, original)
                })?
            }
            Execution::CheckingPrepared(work) => work.advance(|validation| {
                Ok(match validation.advance(maximum_steps)? {
                    BackupImageValidationStep::Pending => BackupRestoreWorkStep::Pending,
                    BackupImageValidationStep::Complete => BackupRestoreWorkStep::Complete,
                })
            })?,
            Execution::Active(mut job) => {
                job.publish()?;
                return self.open_published(retained, control).map(Some);
            }
            Execution::Failed => return Err(uqa_storage::backup::BackupError::Finished),
        };
        match step {
            PreparedRestoreStep::Pending(work) => {
                self.execution = Execution::CheckingPrepared(work);
                Ok(None)
            }
            PreparedRestoreStep::Complete(mut operation) => {
                operation.publish()?;
                self.open_published(retained, control).map(Some)
            }
        }
    }

    pub(super) fn cancel(
        &mut self,
        retained: MemoryBudget,
        control: &StorageReadControl,
    ) -> BackupResult<Option<RestoredEngine>> {
        let completed = match std::mem::replace(&mut self.execution, Execution::Failed) {
            Execution::Admitted(mut operation) => operation.cancel()?,
            Execution::CheckingPrepared(work) => work.cancel()?,
            Execution::Active(mut job) => job.cancel()?,
            Execution::Failed => return Err(uqa_storage::backup::BackupError::Finished),
        };
        if completed.is_some() {
            self.open_published(retained, control)?.finish().map(Some)
        } else {
            Ok(None)
        }
    }

    fn open_published(
        &self,
        retained: MemoryBudget,
        control: &StorageReadControl,
    ) -> BackupResult<PublishedInput> {
        PublishedInput::open(
            self.target.as_ref(),
            self.directory.as_path(),
            self.request,
            &self.options,
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
    options: &AdmittedRestore,
    control: &StorageReadControl,
) -> BackupResult<Option<RestoredEngine>> {
    let operation = repository.cancel_restore(id, directory, control)?;
    let request = operation.request();
    if operation.completed().is_none() {
        return Ok(None);
    }
    PublishedInput::open(
        target,
        directory,
        request,
        options,
        MemoryBudget::new(options.limits.retained_bytes),
        control,
    )?
    .finish()
    .map(Some)
}

struct Factory {
    target: Arc<dyn PersistentBackupTarget>,
    options: AdmittedRestore,
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
            maximum_workspace_bytes: self.options.limits.maximum_workspace_bytes,
            maximum_value_bytes: self.options.limits.maximum_value_bytes,
        }
    }

    fn work<'a>(
        &self,
        input: OwnedPersistentBackupImport<'a>,
        control: &StorageReadControl,
    ) -> Work<'a> {
        Work {
            stage: Stage::Reading(input),
            validator: Validator::new(&self.options),
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
    validator: Validator,
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
                match input.advance_validation(&mut self.validator, 1)? {
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
    bindings: RestoreBindings,
}

impl Validator {
    fn new(settings: &AdmittedRestore) -> Self {
        Self {
            retention: MemoryBudget::new(settings.limits.retained_bytes),
            limits: catalog_limits(settings.limits),
            bindings: settings.bindings.clone(),
        }
    }
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
            Validation::new(
                provider,
                self.retention.clone(),
                self.limits,
                &self.bindings,
                control,
            )
        })
    }
}

struct Validation {
    stage: Option<ValidationStage>,
    control: StorageReadControl,
}

enum ValidationStage {
    Catalog(crate::open::PersistentEngineInput),
    Relationships(
        crate::open::PersistentEngineState,
        crate::backup::validation::CatalogValidation,
    ),
}

impl Validation {
    fn new(
        provider: Arc<dyn PersistentStorageProvider>,
        retention: MemoryBudget,
        limits: CatalogInputLimits,
        bindings: &RestoreBindings,
        control: &StorageReadControl,
    ) -> BackupResult<Self> {
        let input = crate::open::PersistentEngineInput::new(
            provider, retention, limits, bindings, control,
        )?;
        Ok(Self {
            stage: Some(ValidationStage::Catalog(input)),
            control: control.clone(),
        })
    }
}

impl BackupImageValidationWork for Validation {
    fn advance(&mut self, maximum_steps: usize) -> BackupResult<BackupImageValidationStep> {
        let next = match self.stage.take().expect("active private validation") {
            ValidationStage::Catalog(mut input) => match input.advance(maximum_steps)? {
                Some(session) => {
                    let checks = crate::backup::validation::CatalogValidation::begin(
                        session.engine(),
                        &self.control,
                    )?;
                    ValidationStage::Relationships(session, checks)
                }
                None => ValidationStage::Catalog(input),
            },
            ValidationStage::Relationships(session, checks) => {
                let Some(checks) =
                    checks.advance(session.engine(), &self.control, maximum_steps)?
                else {
                    drop(session.finish()?);
                    return Ok(BackupImageValidationStep::Complete);
                };
                ValidationStage::Relationships(session, checks)
            }
        };
        self.stage = Some(next);
        Ok(BackupImageValidationStep::Pending)
    }
}

#[cfg(test)]
mod tests;
