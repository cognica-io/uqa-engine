//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Installed provider destinations retain private import ownership through logical validation.

use super::{BackupError, BackupLease, BackupResult, ImageProvider, RestoreDirectory};
use crate::{mvcc::DatabaseRestore, read_control::StorageReadControl, PersistentStorageProvider};
use std::{path::Path, sync::Arc};
use uqa_core::memory::Budgeted;

mod validation;
pub use validation::{
    BackupImageValidationStep, BackupImageValidationWork, OwnedBackupImageValidation,
};

#[derive(Clone, Copy, Debug)]
pub struct BackupImportLimits {
    pub maximum_workspace_bytes: u64,
    pub maximum_value_bytes: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackupImageImportStep {
    Pending,
    /// Physical input and history transition are complete, but logical validation and publication have not run.
    Imported,
}

/// The enclosing database owner validates logical catalogs and artifacts without executing source commands. Synchronous callbacks close every provider/session handle before returning; a retained continuation owns those handles until it completes or fails.
pub trait BackupImageValidator {
    fn validate(
        &mut self,
        provider: Arc<dyn PersistentStorageProvider>,
        control: &StorageReadControl,
    ) -> BackupResult<()>;

    /// Start one independently retained validation. Existing synchronous validators finish before returning; resumable validators return an admitted continuation that owns their private state and provider handles.
    fn begin(
        &mut self,
        provider: Arc<dyn PersistentStorageProvider>,
        control: &StorageReadControl,
    ) -> BackupResult<OwnedBackupImageValidation> {
        self.validate(provider, control)?;
        OwnedBackupImageValidation::completed(control)
    }
}

/// A private physical input retains its directory binding through the final database validation. It admits validation only after complete input, closes every provider handle before success and retires itself on errors or unwinding. This interface never publishes the destination.
pub trait PersistentBackupImport {
    /// Hash at most the supplied nonzero byte window in one whole-object verification step. Provider record/value and frame bounds apply independently.
    fn advance(&mut self, maximum_object_bytes: usize) -> BackupResult<BackupImageImportStep>;
    fn validate(&mut self, validator: &mut dyn BackupImageValidator) -> BackupResult<()>;

    /// Advance logical validation under the same private file binding. Built-in destinations retain the first returned continuation, so later calls neither replace it nor reopen its provider.
    fn advance_validation(
        &mut self,
        validator: &mut dyn BackupImageValidator,
        maximum_steps: usize,
    ) -> BackupResult<BackupImageValidationStep> {
        if maximum_steps == 0 {
            return Err(BackupError::Invalid("zero validation step limit"));
        }
        self.validate(validator)?;
        Ok(BackupImageValidationStep::Complete)
    }
}

/// Admit the concrete importer header before opening any provider files. The original reservation remains attached until validation completes or the importer retires.
pub struct OwnedPersistentBackupImport<'a> {
    state: State<'a>,
    control: StorageReadControl,
}

enum State<'a> {
    Reading(Budgeted<Box<dyn PersistentBackupImport + 'a>>),
    Imported(Budgeted<Box<dyn PersistentBackupImport + 'a>>),
    Validating(Budgeted<Box<dyn PersistentBackupImport + 'a>>),
    Validated,
    Failed,
}

impl<'a> OwnedPersistentBackupImport<'a> {
    pub fn create<T: PersistentBackupImport + 'a>(
        control: &StorageReadControl,
        create: impl FnOnce() -> BackupResult<T>,
    ) -> BackupResult<Self> {
        control.check()?;
        let memory = control.memory().reserve(size_of::<T>())?;
        let input = create()?;
        control.check()?;
        Ok(Self {
            state: State::Reading(Budgeted::new(Box::new(input), memory)),
            control: control.clone(),
        })
    }

    pub fn advance(&mut self, maximum_object_bytes: usize) -> BackupResult<BackupImageImportStep> {
        match self.state {
            State::Imported(_) | State::Validating(_) | State::Validated => {
                return Ok(BackupImageImportStep::Imported)
            }
            State::Failed => return Err(BackupError::Finished),
            State::Reading(_) => (),
        }
        let State::Reading(mut input) = std::mem::replace(&mut self.state, State::Failed) else {
            unreachable!()
        };
        if maximum_object_bytes == 0 {
            return Err(BackupError::Invalid("zero object verification window"));
        }
        self.control.check()?;
        let step = input.as_mut().advance(maximum_object_bytes)?;
        self.control.check()?;
        self.state = match step {
            BackupImageImportStep::Pending => State::Reading(input),
            BackupImageImportStep::Imported => State::Imported(input),
        };
        Ok(step)
    }

    /// Complete validation synchronously through the same retained transition used by bounded callers.
    pub fn validate(&mut self, validator: &mut dyn BackupImageValidator) -> BackupResult<()> {
        while self.advance_validation(validator, 64)? == BackupImageValidationStep::Pending {}
        Ok(())
    }

    pub fn advance_validation(
        &mut self,
        validator: &mut dyn BackupImageValidator,
        maximum_steps: usize,
    ) -> BackupResult<BackupImageValidationStep> {
        match self.state {
            State::Validated => return Ok(BackupImageValidationStep::Complete),
            State::Failed => return Err(BackupError::Finished),
            State::Reading(_) => {
                return Err(BackupError::Invalid("physical image import is incomplete"))
            }
            State::Imported(_) | State::Validating(_) => (),
        }
        let (State::Imported(mut input) | State::Validating(mut input)) =
            std::mem::replace(&mut self.state, State::Failed)
        else {
            unreachable!()
        };
        if maximum_steps == 0 {
            return Err(BackupError::Invalid("zero validation step limit"));
        }
        self.control.check()?;
        let step = input
            .as_mut()
            .advance_validation(validator, maximum_steps)?;
        self.control.check()?;
        self.state = match step {
            BackupImageValidationStep::Pending => State::Validating(input),
            BackupImageValidationStep::Complete => {
                drop(input);
                State::Validated
            }
        };
        Ok(step)
    }
}

/// An installed provider's destination configuration, independent of Engine and of an already-open source database. Provider-owned options select physical protection; archive bytes cannot select a pathname or provider implementation.
pub trait PersistentBackupTarget: Send + Sync {
    fn image_provider(&self) -> ImageProvider;

    fn import<'a>(
        &self,
        directory: &RestoreDirectory<'_>,
        backup: BackupLease<'a>,
        request: DatabaseRestore,
        limits: BackupImportLimits,
        control: &StorageReadControl,
    ) -> BackupResult<OwnedPersistentBackupImport<'a>>;

    fn recover<'a>(
        &self,
        directory: &RestoreDirectory<'_>,
        backup: BackupLease<'a>,
        request: DatabaseRestore,
        limits: BackupImportLimits,
        control: &StorageReadControl,
    ) -> BackupResult<OwnedPersistentBackupImport<'a>>;

    /// Open the installed provider after the enclosing restore owner has published its directory, resolving the exact durable request without creating a missing file or replacing later history.
    fn open(
        &self,
        directory: &Path,
        request: DatabaseRestore,
        control: &StorageReadControl,
    ) -> BackupResult<Arc<dyn PersistentStorageProvider>>;
}

#[cfg(test)]
mod tests;
