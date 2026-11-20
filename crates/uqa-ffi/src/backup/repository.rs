//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::{ffi::c_char, sync::Arc};
use uqa_engine::backup::{
    BackupKey, BackupLimits, BackupListing, LocalRepository, Protection, ProtectionId,
    StorageReadControl,
};

use super::{options, UQABackupId, UQABackupOptions, UQABackupRepositoryOptions};
use crate::{
    abi::{UQA_DONE, UQA_OK},
    boundary,
};

#[repr(C)]
pub struct UQABackupEncryption {
    pub domain: [u8; 16],
    pub key: [u8; 32],
}

pub struct UQABackupRepository {
    pub(super) native: Arc<LocalRepository>,
    memory_limit: usize,
    workspace_limit: u64,
}

impl UQABackupRepository {
    fn control(&self) -> StorageReadControl {
        StorageReadControl::with_limit(self.memory_limit)
    }
}

macro_rules! repository_handle {
    ($api:ident, $method:ident, $read:ident $(, $size:ident)?) => {
        ffi_fn!($api(
            path: *const c_char, $($size: usize,)? encryption: *const UQABackupEncryption,
            options: *const UQABackupRepositoryOptions, out: *mut *mut UQABackupRepository
        ) {
            let out = boundary::new_handle(out)?;
            let path = boundary::$read(path $(, $size)?)?;
            let options = options::optional(options)?;
            let memory_limit = options::memory(options.memory_limit_bytes)?;
            let protection = if encryption.is_null() {
                Protection::Plaintext
            } else {
                let encryption = boundary::input(encryption)?;
                Protection::Encrypted {
                    domain: ProtectionId::from_bytes(encryption.domain)?,
                    key: BackupKey::new(encryption.key),
                }
            };
            let limits = BackupLimits::new(options.maximum_key_bytes, options.maximum_frame_bytes)?;
            let control = StorageReadControl::with_limit(memory_limit);
            *out = Box::into_raw(boundary::boxed_with(|| {
                let native = LocalRepository::$method(path, protection, limits, &control)?;
                Ok(UQABackupRepository {
                    native: Arc::new(native), memory_limit, workspace_limit: options.maximum_workspace_bytes,
                })
            })?);
            Ok(UQA_OK)
        });
    };
}
repository_handle!(uqa_backup_repository_create, create, text);
repository_handle!(
    uqa_backup_repository_create_n,
    create,
    required_text_n,
    path_size
);
repository_handle!(uqa_backup_repository_open, open, text);
repository_handle!(
    uqa_backup_repository_open_n,
    open,
    required_text_n,
    path_size
);
ffi_fn!(uqa_backup_repository_close(repository: *mut *mut UQABackupRepository) { boundary::release(repository) });

ffi_fn!(uqa_backup_repository_verify(repository: *mut UQABackupRepository, id: *const UQABackupId) {
    let repo = boundary::input(repository)?;
    let control = repo.control();
    let lease = repo.native.open_backup(boundary::input(id)?.native()?, &control)?;
    lease.verify(repo.workspace_limit, &control)?;
    Ok(UQA_OK)
});
ffi_fn!(uqa_backup_repository_remove(repository: *mut UQABackupRepository, id: *const UQABackupId) {
    let repo = boundary::input(repository)?;
    repo.native.remove_backup(boundary::input(id)?.native()?, &repo.control())?;
    Ok(UQA_OK)
});
ffi_fn!(uqa_backup_repository_resume_publication(repository: *mut UQABackupRepository, id: *const UQABackupId) {
    let repo = boundary::input(repository)?;
    let control = repo.control();
    repo.native.resume_publication(boundary::input(id)?.native()?, repo.workspace_limit, &control)?
        .publish(&control)?;
    Ok(UQA_OK)
});

ffi_fn!(uqa_backup_repository_copy(
    source: *mut UQABackupRepository, id: *const UQABackupId, destination: *mut UQABackupRepository,
    options: *const UQABackupOptions, out: *mut UQABackupId
) {
    let id = boundary::input(id).copied();
    let out = boundary::output(out)?;
    let source = boundary::input(source)?;
    let destination = boundary::input(destination)?;
    let options = options::optional(options)?;
    let control = StorageReadControl::with_limit(options::memory(options.memory_limit_bytes)?);
    let copy = options.native()?;
    let root = source.native.copy_backup_to(
        id?.native()?, &destination.native, copy.maximum_encoded_bytes, copy.maximum_workspace_bytes,
        copy.allow_plaintext_export, &control,
    )?;
    *out = root.backup().into();
    Ok(UQA_OK)
});

#[repr(C)]
#[derive(Default)]
pub struct UQABackupCollectionReport {
    pub completed_backups: u64,
    pub prepared_backups: u64,
    pub active_operations: u64,
    pub referenced_frame_bytes: u64,
    pub retained_object_bytes: u64,
    pub unreferenced_object_bytes: u64,
    pub removed_objects: u64,
    pub removed_object_bytes: u64,
    pub removed_staging_files: u64,
    pub removed_staging_bytes: u64,
    pub remaining_files: u64,
    pub workspace_peak_bytes: u64,
}

ffi_fn!(uqa_backup_repository_collect_garbage(repository: *mut UQABackupRepository, maximum_files: usize, out: *mut UQABackupCollectionReport) {
    let out = boundary::output(out)?;
    let repo = boundary::input(repository)?;
    let report = repo.native.collect_garbage(maximum_files, repo.workspace_limit, &repo.control())?;
    *out = UQABackupCollectionReport {
        completed_backups: report.completed_backups,
        prepared_backups: report.prepared_backups,
        active_operations: report.active_operations,
        referenced_frame_bytes: report.referenced_frame_bytes,
        retained_object_bytes: report.retained_object_bytes,
        unreferenced_object_bytes: report.unreferenced_object_bytes,
        removed_objects: report.removed_objects,
        removed_object_bytes: report.removed_object_bytes,
        removed_staging_files: report.removed_staging_files,
        removed_staging_bytes: report.removed_staging_bytes,
        remaining_files: report.remaining_files,
        workspace_peak_bytes: report.workspace_peak_bytes,
    };
    Ok(UQA_OK)
});

pub struct UQABackupList(BackupListing);
ffi_fn!(uqa_backup_list_create(repository: *mut UQABackupRepository, out: *mut *mut UQABackupList) {
    let out = boundary::new_handle(out)?;
    let repo = boundary::input(repository)?;
    let list = repo.native.list_backups(repo.workspace_limit, &repo.control())?;
    *out = Box::into_raw(boundary::boxed(UQABackupList(list))?);
    Ok(UQA_OK)
});
ffi_fn!(uqa_backup_list_next(list: *mut UQABackupList, id: *mut UQABackupId, parent: *mut UQABackupId) {
    if id == parent { return Err(crate::error::UQAError::invalid("backup ID and parent require separate outputs")); }
    let id = boundary::output(id)?;
    let parent = boundary::output(parent)?;
    let Some(root) = boundary::input_mut(list)?.0.next_backup()? else { return Ok(UQA_DONE) };
    *id = root.backup().into();
    *parent = root.parent().map(Into::into).unwrap_or_default();
    Ok(UQA_OK)
});
ffi_fn!(uqa_backup_list_destroy(list: *mut *mut UQABackupList) { boundary::release(list) });
