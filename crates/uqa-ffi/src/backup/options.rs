//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! C option layouts and native default conversion.

use crate::{abi::*, boundary, error::UQAError};
use uqa_engine::backup::{
    BackupLimits, BackupOptions, RestoreOptions, DEFAULT_BACKUP_MEMORY_BYTES,
    DEFAULT_BACKUP_WORKSPACE_BYTES,
};

#[repr(C)]
#[derive(Clone, Copy)]
pub struct UQABackupRepositoryOptions {
    pub struct_size: u32,
    pub version: u32,
    pub maximum_key_bytes: u32,
    pub maximum_frame_bytes: u32,
    pub memory_limit_bytes: u64,
    pub maximum_workspace_bytes: u64,
}

impl Default for UQABackupRepositoryOptions {
    fn default() -> Self {
        let limits = BackupLimits::default();
        Self {
            struct_size: size_of::<Self>() as u32,
            version: 1,
            maximum_key_bytes: limits.max_key_bytes(),
            maximum_frame_bytes: limits.max_frame_bytes(),
            memory_limit_bytes: DEFAULT_BACKUP_MEMORY_BYTES as u64,
            maximum_workspace_bytes: DEFAULT_BACKUP_WORKSPACE_BYTES,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct UQABackupOptions {
    pub struct_size: u32,
    pub version: u32,
    pub memory_limit_bytes: u64,
    pub maximum_encoded_bytes: u64,
    pub maximum_workspace_bytes: u64,
    pub allow_plaintext_export: u32,
    pub reserved: u32,
}

impl Default for UQABackupOptions {
    fn default() -> Self {
        let options = BackupOptions::default();
        Self {
            struct_size: size_of::<Self>() as u32,
            version: 1,
            memory_limit_bytes: DEFAULT_BACKUP_MEMORY_BYTES as u64,
            maximum_encoded_bytes: options.maximum_encoded_bytes,
            maximum_workspace_bytes: options.maximum_workspace_bytes,
            allow_plaintext_export: u32::from(options.allow_plaintext_export),
            reserved: 0,
        }
    }
}

impl UQABackupOptions {
    pub(super) fn native(self) -> boundary::Result<BackupOptions> {
        if self.allow_plaintext_export > 1 || self.reserved != 0 {
            return Err(UQAError::invalid(
                "invalid backup option flag or reserved field",
            ));
        }
        let mut options =
            BackupOptions::new(self.maximum_encoded_bytes, self.maximum_workspace_bytes);
        options.allow_plaintext_export = self.allow_plaintext_export != 0;
        Ok(options)
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct UQARestoreOptions {
    pub struct_size: u32,
    pub version: u32,
    pub memory_limit_bytes: u64,
    pub maximum_workspace_bytes: u64,
    pub maximum_value_bytes: u64,
    pub retained_bytes: u64,
}

impl Default for UQARestoreOptions {
    fn default() -> Self {
        let options = RestoreOptions::default();
        Self {
            struct_size: size_of::<Self>() as u32,
            version: 1,
            memory_limit_bytes: DEFAULT_BACKUP_MEMORY_BYTES as u64,
            maximum_workspace_bytes: options.maximum_workspace_bytes,
            maximum_value_bytes: options.maximum_value_bytes as u64,
            retained_bytes: options.retained_bytes as u64,
        }
    }
}

impl UQARestoreOptions {
    pub(super) fn native(self) -> boundary::Result<RestoreOptions> {
        Ok(RestoreOptions {
            maximum_workspace_bytes: self.maximum_workspace_bytes,
            maximum_value_bytes: memory(self.maximum_value_bytes)?,
            retained_bytes: memory(self.retained_bytes)?,
        })
    }
}

pub(super) fn memory(bytes: u64) -> boundary::Result<usize> {
    let bytes = usize::try_from(bytes)
        .map_err(|_| UQAError::new(UQA_ERROR_RANGE, "backup limit exceeds addressable range"))?;
    if bytes == 0 {
        return Err(UQAError::invalid(
            "backup memory and value limits must be positive",
        ));
    }
    Ok(bytes)
}

// Called only with option types that start with the ABI size and version pair.
#[allow(unsafe_code)]
pub(super) unsafe fn optional<T: Default + Copy>(options: *const T) -> boundary::Result<T> {
    if options.is_null() {
        Ok(T::default())
    } else {
        Ok(*boundary::versioned_options(options)?)
    }
}

macro_rules! initialize {
    ($name:ident, $option:ty) => {
        ffi_fn!($name(out: *mut $option, size: usize) {
            if size < size_of::<$option>() {
                return Err(UQAError::invalid("backup option storage is too small"));
            }
            boundary::output(out)?;
            Ok(UQA_OK)
        });
    };
}
initialize!(
    uqa_backup_repository_options_init,
    UQABackupRepositoryOptions
);
initialize!(uqa_backup_options_init, UQABackupOptions);
initialize!(uqa_restore_options_init, UQARestoreOptions);
