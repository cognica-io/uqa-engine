//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Real destination validation retains one private provider across advances and retires it before publication or recovery.

use crate::{
    backup::{
        BackupError, BackupImageImportStep, BackupImageSource, BackupImageValidationStep,
        BackupImageValidationWork, BackupImageValidator, BackupImportLimits, BackupLease,
        BackupLimits, BackupRestoreFactory, BackupRestoreJob, BackupRestoreStep, BackupRestoreWork,
        BackupRestoreWorkStep, BackupResult, BackupRoot, LocalRepository,
        OwnedBackupImageValidation, OwnedPersistentBackupImport, PersistentBackupTarget,
        Protection, RestoreDirectory,
    },
    mvcc::DatabaseRestore,
    read_control::StorageReadControl,
    PersistentStorageProvider,
};
use std::{
    path::Path,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Weak,
    },
};

#[derive(Default)]
struct Trace {
    starts: AtomicUsize,
    calls: AtomicUsize,
    closed: AtomicUsize,
    provider: parking_lot::Mutex<Option<Weak<dyn PersistentStorageProvider>>>,
}

struct Validation {
    provider: Arc<dyn PersistentStorageProvider>,
    trace: Arc<Trace>,
    remaining: usize,
}

impl Drop for Validation {
    fn drop(&mut self) {
        self.trace.closed.fetch_add(1, Ordering::SeqCst);
    }
}

impl BackupImageValidationWork for Validation {
    fn advance(&mut self, maximum_steps: usize) -> BackupResult<BackupImageValidationStep> {
        require(
            maximum_steps == 1,
            "validation receives its exact step window",
        )?;
        self.trace.calls.fetch_add(1, Ordering::SeqCst);
        let session = self.provider.open_session()?;
        require(
            session
                .catalog
                .get_metadata("validation_fixture")?
                .as_deref()
                == Some("original"),
            "validation retains the same restored catalog",
        )?;
        self.remaining -= 1;
        Ok(if self.remaining == 0 {
            BackupImageValidationStep::Complete
        } else {
            BackupImageValidationStep::Pending
        })
    }
}

struct Validator(Arc<Trace>);

impl BackupImageValidator for Validator {
    fn validate(
        &mut self,
        _: Arc<dyn PersistentStorageProvider>,
        _: &StorageReadControl,
    ) -> BackupResult<()> {
        Err(BackupError::Invalid(
            "destination must start retained validation",
        ))
    }

    fn begin(
        &mut self,
        provider: Arc<dyn PersistentStorageProvider>,
        control: &StorageReadControl,
    ) -> BackupResult<OwnedBackupImageValidation> {
        self.0.starts.fetch_add(1, Ordering::SeqCst);
        *self.0.provider.lock() = Some(Arc::downgrade(&provider));
        OwnedBackupImageValidation::create(control, || {
            Ok(Validation {
                provider,
                trace: Arc::clone(&self.0),
                remaining: 3,
            })
        })
    }
}

struct Factory {
    target: Arc<dyn PersistentBackupTarget>,
    trace: Arc<Trace>,
}

struct Driver<'a> {
    input: OwnedPersistentBackupImport<'a>,
    validator: Validator,
    reading: bool,
}

impl<'a> BackupRestoreFactory<'a> for Factory {
    type Work = Driver<'a>;
    fn import(
        self,
        directory: &RestoreDirectory<'_>,
        backup: BackupLease<'a>,
        request: DatabaseRestore,
        control: &StorageReadControl,
    ) -> BackupResult<Self::Work> {
        let input = self
            .target
            .import(directory, backup, request, limits(), control)?;
        Ok(Driver {
            input,
            validator: Validator(self.trace),
            reading: true,
        })
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
            .recover(directory, backup, request, limits(), control)?;
        Ok(Driver {
            input,
            validator: Validator(self.trace),
            reading: true,
        })
    }
}

impl BackupRestoreWork for Driver<'_> {
    fn advance(&mut self) -> BackupResult<BackupRestoreWorkStep> {
        if self.reading {
            if self.input.advance(4096)? == BackupImageImportStep::Imported {
                self.reading = false;
            }
            return Ok(BackupRestoreWorkStep::Pending);
        }
        Ok(
            match self.input.advance_validation(&mut self.validator, 1)? {
                BackupImageValidationStep::Pending => BackupRestoreWorkStep::Pending,
                BackupImageValidationStep::Complete => BackupRestoreWorkStep::Complete,
            },
        )
    }
}

fn limits() -> BackupImportLimits {
    BackupImportLimits {
        maximum_workspace_bytes: 8 << 20,
        maximum_value_bytes: 1 << 20,
    }
}
fn require(value: bool, message: &'static str) -> BackupResult<()> {
    if value {
        Ok(())
    } else {
        Err(BackupError::Invalid(message))
    }
}

fn start_validation(job: &mut BackupRestoreJob<'_, Driver<'_>>, trace: &Trace) -> BackupResult<()> {
    for _ in 0..100_000 {
        require(
            job.advance()? == BackupRestoreStep::Pending,
            "validation must remain private while pending",
        )?;
        if trace.calls.load(Ordering::SeqCst) != 0 {
            break;
        }
    }
    require(
        trace.starts.load(Ordering::SeqCst) == 1,
        "destination starts exactly one validator",
    )?;
    require(
        trace.calls.load(Ordering::SeqCst) == 1,
        "one validation step per call",
    )?;
    require(
        trace
            .provider
            .lock()
            .as_ref()
            .and_then(Weak::upgrade)
            .is_some(),
        "pending validation retains its private provider",
    )?;
    require(job.publish().is_err(), "pending validation cannot publish")
}

fn complete_validation(
    job: &mut BackupRestoreJob<'_, Driver<'_>>,
    trace: &Trace,
) -> BackupResult<()> {
    require(
        job.advance()? == BackupRestoreStep::Pending,
        "second validation step stays pending",
    )?;
    require(
        job.advance()? == BackupRestoreStep::Prepared,
        "third validation step prepares publication",
    )?;
    require(
        trace.starts.load(Ordering::SeqCst) == 1 && trace.calls.load(Ordering::SeqCst) == 3,
        "the original validator survives every step",
    )?;
    require(
        trace.closed.load(Ordering::SeqCst) == 1,
        "preparation closes validation work",
    )?;
    require(
        trace
            .provider
            .lock()
            .as_ref()
            .and_then(Weak::upgrade)
            .is_none(),
        "preparation closes every private provider handle",
    )?;
    job.publish()?;
    Ok(())
}

/// Verify retained validation and cancellation recovery against a fresh disposable installed provider and destination configuration.
pub fn verify_backup_validation_continuation(
    source: Arc<dyn PersistentStorageProvider>,
    target: &Arc<dyn PersistentBackupTarget>,
    directory: &Path,
) -> BackupResult<()> {
    let (repository, root) = capture_original(source, directory)?;
    for cancel in [false, true] {
        let control = StorageReadControl::with_limit(32 << 20);
        let trace = Arc::new(Trace::default());
        let path = directory.join(if cancel { "cancelled" } else { "completed" });
        let operation = repository.begin_restore(root.backup(), &path, &control)?;
        let mut job = operation.into_job(Factory {
            target: Arc::clone(target),
            trace: Arc::clone(&trace),
        })?;
        let id = job.id();
        let request = job.request();
        start_validation(&mut job, &trace)?;
        require(
            target.open(&path, request, &control).is_err(),
            "private validation is not a published database",
        )?;
        if cancel {
            control.cancellation().cancel();
            require(
                job.advance().is_err(),
                "original cancellation stops validation",
            )?;
            require(
                trace.closed.load(Ordering::SeqCst) == 1,
                "cancelled validation is retired",
            )?;
            require(
                trace
                    .provider
                    .lock()
                    .as_ref()
                    .and_then(Weak::upgrade)
                    .is_none(),
                "cancelled validation closes its provider",
            )?;
            drop(job);
            require(
                control.memory().used() == 0,
                "cancelled import releases its original allowance",
            )?;
            let recovery = StorageReadControl::with_limit(32 << 20);
            let trace = Arc::new(Trace::default());
            let operation = repository.resume_restore(id, &path, &recovery)?;
            let mut job = operation.into_job(Factory {
                target: Arc::clone(target),
                trace: Arc::clone(&trace),
            })?;
            start_validation(&mut job, &trace)?;
            complete_validation(&mut job, &trace)?;
            drop(job);
            let restored = target.open(&path, request, &recovery)?;
            require(
                restored
                    .open_session()?
                    .catalog
                    .get_metadata("validation_fixture")?
                    .as_deref()
                    == Some("original"),
                "recovery preserves the original private image",
            )?;
            drop(restored);
            require(
                recovery.memory().used() == 0,
                "recovered validation releases its allowance",
            )?;
        } else {
            complete_validation(&mut job, &trace)?;
            drop(job);
            let restored = target.open(&path, request, &control)?;
            require(
                restored
                    .open_session()?
                    .catalog
                    .get_metadata("validation_fixture")?
                    .as_deref()
                    == Some("original"),
                "validated publication preserves the original catalog",
            )?;
            drop(restored);
            require(
                control.memory().used() == 0,
                "completed validation releases its allowance",
            )?;
        }
    }
    Ok(())
}

fn capture_original(
    source: Arc<dyn PersistentStorageProvider>,
    directory: &Path,
) -> BackupResult<(LocalRepository, BackupRoot)> {
    let session = source.open_session()?;
    session.catalog.initialize_storage()?;
    session
        .catalog
        .set_metadata("validation_fixture", "original")?;
    drop(session);
    let capture = StorageReadControl::with_limit(32 << 20);
    let image = source.capture_backup_image(&capture)?;
    let repository = LocalRepository::create(
        directory.join("repository"),
        Protection::Plaintext,
        BackupLimits::new(4096, 4096)?,
        &capture,
    )?;
    let root = {
        let mut operation = repository.begin_backup(image.cut(), None, 16 << 20, &capture)?;
        operation.prepare_image(image, 8 << 20, &capture)?;
        operation.publish(&capture)?
    };
    drop(source);
    Ok((repository, root))
}
