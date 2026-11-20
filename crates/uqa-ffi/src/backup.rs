//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native C handles for committed backups and publication into a new Engine.

mod options;
mod repository;
mod restore;

pub use options::*;
pub use repository::*;
pub use restore::*;

use crate::{
    abi::{
        UQAStatus, UQA_ERROR_BUSY, UQA_ERROR_CANCELLED, UQA_ERROR_INTEGRITY,
        UQA_ERROR_INVALID_ARGUMENT, UQA_ERROR_IO, UQA_ERROR_LIMIT, UQA_ERROR_STATE,
        UQA_ERROR_UNSUPPORTED, UQA_OK,
    },
    boundary,
    error::UQAError,
    session::UQAEngine,
};
use uqa_engine::backup::{
    BackupCapturePhase, BackupError, BackupId, OwnedBackupJob, StorageReadControl,
};

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct UQABackupId {
    pub bytes: [u8; 16],
}

impl From<BackupId> for UQABackupId {
    fn from(id: BackupId) -> Self {
        Self {
            bytes: id.as_bytes(),
        }
    }
}

impl UQABackupId {
    fn native(self) -> boundary::Result<BackupId> {
        Ok(BackupId::from_bytes(self.bytes)?)
    }
}

impl From<BackupError> for UQAError {
    fn from(error: BackupError) -> Self {
        Self::display(backup_status(&error), &error)
    }
}

fn backup_status(error: &BackupError) -> UQAStatus {
    match error {
        BackupError::Invalid(_) => UQA_ERROR_INVALID_ARGUMENT,
        BackupError::Unsupported(_) => UQA_ERROR_UNSUPPORTED,
        BackupError::Integrity => UQA_ERROR_INTEGRITY,
        BackupError::Finished => UQA_ERROR_STATE,
        BackupError::Busy => UQA_ERROR_BUSY,
        BackupError::Limit { .. } => UQA_ERROR_LIMIT,
        BackupError::Memory(memory) => UQAError::from(memory.clone()).status,
        BackupError::Cancelled(_) => UQA_ERROR_CANCELLED,
        BackupError::Io(_) | BackupError::Storage(_) | BackupError::Random => UQA_ERROR_IO,
    }
}

fn admission(error: uqa_sql::SQLError) -> UQAError {
    use uqa_sql::SQLError;
    let status = match error.primary() {
        SQLError::Host(source) => source.downcast_ref::<BackupError>().map(backup_status),
        SQLError::Memory(memory) => Some(UQAError::from(memory.clone()).status),
        SQLError::Cancelled(_) => Some(UQA_ERROR_CANCELLED),
        _ => None,
    };
    let mut diagnostic = UQAError::from(error);
    if let Some(status) = status {
        diagnostic.status = status;
    }
    diagnostic
}

fn capture_phase(phase: BackupCapturePhase) -> u32 {
    match phase {
        BackupCapturePhase::Capturing => 1,
        BackupCapturePhase::Verifying => 2,
        BackupCapturePhase::Prepared => 3,
        BackupCapturePhase::Complete => 4,
        BackupCapturePhase::Cancelling => 5,
        BackupCapturePhase::Cancelled => 6,
        BackupCapturePhase::Failed => 7,
    }
}

pub struct UQABackupJob(OwnedBackupJob);

ffi_fn!(uqa_backup_job_create(
    engine: *mut UQAEngine, repository: *mut UQABackupRepository, parent: *const UQABackupId,
    options: *const UQABackupOptions, out: *mut *mut UQABackupJob
) {
    let out = boundary::new_handle(out)?;
    let session = &boundary::input(engine)?.session;
    let _call = session.enter()?;
    let repository = &boundary::input(repository)?.native;
    let options = options::optional(options)?;
    let mut backup = options.native()?;
    if !parent.is_null() {
        backup.parent = Some(boundary::input(parent)?.native()?);
    }
    let control = StorageReadControl::with_limit(options::memory(options.memory_limit_bytes)?);
    *out = Box::into_raw(boundary::boxed_with(|| {
        let job = OwnedBackupJob::begin(&session.native, repository.clone(), backup, &control)
            .map_err(admission)?;
        Ok(UQABackupJob(job))
    })?);
    Ok(UQA_OK)
});
ffi_fn!(uqa_backup_job_get_id(job: *mut UQABackupJob, out: *mut UQABackupId) {
    let out = boundary::output(out)?;
    *out = boundary::input(job)?.0.id().into();
    Ok(UQA_OK)
});
ffi_fn!(uqa_backup_job_get_phase(job: *mut UQABackupJob, out: *mut u32) {
    let out = boundary::output(out)?;
    *out = capture_phase(boundary::input(job)?.0.progress().phase);
    Ok(UQA_OK)
});
ffi_fn!(uqa_backup_job_advance(job: *mut UQABackupJob, maximum_positions: usize, maximum_bytes: usize, out: *mut u32) {
    let out = boundary::output(out)?;
    let job = &mut boundary::input_mut(job)?.0;
    job.advance(maximum_positions, maximum_bytes)?;
    *out = capture_phase(job.progress().phase);
    Ok(UQA_OK)
});
ffi_fn!(uqa_backup_job_publish(job: *mut UQABackupJob, out: *mut UQABackupId) {
    let out = boundary::output(out)?;
    *out = boundary::input_mut(job)?.0.publish()?.backup().into();
    Ok(UQA_OK)
});
ffi_fn!(uqa_backup_job_finish(job: *mut UQABackupJob, out: *mut UQABackupId) {
    let out = boundary::output(out)?;
    *out = boundary::input_mut(job)?.0.finish()?.backup().into();
    Ok(UQA_OK)
});
ffi_fn!(uqa_backup_job_cancel(job: *mut UQABackupJob) {
    let _ = boundary::input_mut(job)?.0.cancel()?;
    Ok(UQA_OK)
});
ffi_fn!(uqa_backup_job_destroy(job: *mut *mut UQABackupJob) { boundary::release(job) });
