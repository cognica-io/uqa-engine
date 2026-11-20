//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::ffi::c_char;
use uqa_engine::backup::{
    OwnedRestoreJob, RestoreDestination, RestoreId, RestorePhase, SQLiteBackupMapping,
    SQLiteBackupTarget, StorageEncryptionKey, StorageReadControl,
};

use super::{options, UQABackupId, UQABackupRepository, UQARestoreOptions};
use crate::{
    abi::*,
    boundary,
    config::UQAConfig,
    error::UQAError,
    session::{publish_restored, PersistentBackend, UQAEngine},
};

#[repr(C)]
#[derive(Default, Clone, Copy)]
pub struct UQARestoreId {
    pub bytes: [u8; 16],
}

impl UQARestoreId {
    fn native(self) -> boundary::Result<RestoreId> {
        Ok(RestoreId::from_bytes(self.bytes)?)
    }
}

pub struct UQARestoreJob {
    native: OwnedRestoreJob,
    persistent: bool,
    ffi_memory_limit: usize,
}

fn phase(value: RestorePhase) -> u32 {
    match value {
        RestorePhase::Reading => 1,
        RestorePhase::Validating => 2,
        RestorePhase::Prepared => 3,
        RestorePhase::Publishing => 4,
        RestorePhase::Complete => 5,
        RestorePhase::Cancelled => 6,
        RestorePhase::Failed => 7,
    }
}

fn destination(
    flags: UQAOpenFlags,
    path: Option<&str>,
    config: &UQAConfig,
) -> boundary::Result<RestoreDestination> {
    if config.trusted_anchor.is_some() {
        return Err(UQAError::invalid(
            "restore establishes a new destination identity and anchor",
        ));
    }
    let Some(path) = path else {
        if flags != UQA_OPEN_MEMORY || config.key.is_some() || config.compression.is_some() {
            return Err(UQAError::invalid(
                "memory restore requires a NULL path and no storage options",
            ));
        }
        return Ok(RestoreDestination::Memory);
    };
    let mapping = match PersistentBackend::from_flags(flags)? {
        PersistentBackend::Redb => {
            if config.key.is_some() || config.compression.is_some() {
                return Err(UQAError::invalid(
                    "redb restore does not support encryption or compression",
                ));
            }
            return Ok(RestoreDestination::persistent(
                path,
                uqa_storage_redb::RedbBackupTarget::default(),
            ));
        }
        PersistentBackend::SQLiteNative { auto: false } => SQLiteBackupMapping::Native,
        PersistentBackend::SQLiteKeyValue { auto: false } => SQLiteBackupMapping::KeyValue,
        _ => {
            return Err(UQAError::invalid(
                "restore requires explicit destination storage options",
            ))
        }
    };
    let mut target = SQLiteBackupTarget::new(mapping);
    target.encryption_key = config
        .key
        .as_ref()
        .map(|key| StorageEncryptionKey::new(key));
    target.compression = config.compression;
    Ok(RestoreDestination::persistent(path, target))
}

macro_rules! restore_handle {
    ($api:ident, $method:ident, $identity:ty, $read:ident $(, $size:ident)?) => {
        ffi_fn!($api(
            engine: *mut UQAEngine, repository: *mut UQABackupRepository, id: *const $identity,
            path: *const c_char, $($size: usize,)? flags: UQAOpenFlags, config: *mut UQAConfig,
            options: *const UQARestoreOptions, out: *mut *mut UQARestoreJob
        ) {
            let out = boundary::new_handle(out)?;
            let session = &boundary::input(engine)?.session;
            let _call = session.enter()?;
            let repository = &boundary::input(repository)?.native;
            let id = boundary::input(id)?.native()?;
            let path = if path.is_null() {
                $(if $size != 0 { return Err(UQAError::invalid("NULL restore path requires zero length")); })?
                None
            } else {
                Some(boundary::$read(path $(, $size)?)?)
            };
            let defaults = UQAConfig::default();
            let config = if config.is_null() { &defaults } else { boundary::input(config)? };
            let destination = destination(flags, path, config)?;
            let options = options::optional(options)?;
            let control = StorageReadControl::with_limit(options::memory(options.memory_limit_bytes)?);
            *out = Box::into_raw(boundary::boxed_with(|| {
                let native = OwnedRestoreJob::$method(
                    &session.native, repository.clone(), id, destination,
                    options.native()?.with_runtime(&session.native), &control,
                ).map_err(super::admission)?;
                Ok(UQARestoreJob {
                    native, persistent: flags != UQA_OPEN_MEMORY, ffi_memory_limit: config.memory_limit,
                })
            })?);
            Ok(UQA_OK)
        });
    };
}
restore_handle!(uqa_restore_job_create, begin, UQABackupId, text);
restore_handle!(
    uqa_restore_job_create_n,
    begin,
    UQABackupId,
    required_text_n,
    path_size
);
restore_handle!(uqa_restore_job_resume, resume, UQARestoreId, text);
restore_handle!(
    uqa_restore_job_resume_n,
    resume,
    UQARestoreId,
    required_text_n,
    path_size
);

ffi_fn!(uqa_restore_job_get_id(job: *mut UQARestoreJob, out: *mut UQARestoreId) {
    let out = boundary::output(out)?;
    let Some(id) = boundary::input(job)?.native.id() else { return Ok(UQA_DONE) };
    out.bytes = id.as_bytes();
    Ok(UQA_OK)
});
ffi_fn!(uqa_restore_job_get_phase(job: *mut UQARestoreJob, out: *mut u32) {
    let out = boundary::output(out)?;
    *out = phase(boundary::input(job)?.native.phase());
    Ok(UQA_OK)
});
ffi_fn!(uqa_restore_job_advance(job: *mut UQARestoreJob, maximum_bytes: usize, out: *mut u32) {
    let out = boundary::output(out)?;
    *out = phase(boundary::input_mut(job)?.native.advance(maximum_bytes)?);
    Ok(UQA_OK)
});
ffi_fn!(uqa_restore_job_publish(job: *mut *mut UQARestoreJob, maximum_steps: usize, out: *mut *mut UQAEngine) {
    let out = boundary::new_handle(out)?;
    let restore = boundary::input_mut(*boundary::input(job)?)?;
    if let Some(engine) = restore.native.advance_publication(maximum_steps)? {
        *out = publish_restored(engine.into_retained(), restore.persistent, restore.ffi_memory_limit)?;
        boundary::release(job)?;
    }
    Ok(UQA_OK)
});
ffi_fn!(uqa_restore_job_finish(job: *mut *mut UQARestoreJob, out: *mut *mut UQAEngine) {
    let out = boundary::new_handle(out)?;
    let restore = boundary::input_mut(*boundary::input(job)?)?;
    let engine = restore.native.finish()?;
    *out = publish_restored(engine.into_retained(), restore.persistent, restore.ffi_memory_limit)?;
    boundary::release(job)
});
ffi_fn!(uqa_restore_job_cancel(job: *mut UQARestoreJob) {
    let _ = boundary::input_mut(job)?.native.cancel()?;
    Ok(UQA_OK)
});
ffi_fn!(uqa_restore_job_destroy(job: *mut *mut UQARestoreJob) { boundary::release(job) });
