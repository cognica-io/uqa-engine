//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use napi::bindgen_prelude::Buffer;
use napi::{Error, Result};
use napi_derive::napi;
use uqa_engine::backup as core;

use crate::input::{compression_options, runtime_error};
use crate::results::CompressionOptions;

fn array<const N: usize>(input: &[u8], name: &str) -> Result<[u8; N]> {
    input
        .try_into()
        .map_err(|_| Error::from_reason(format!("{name} must contain {N} bytes")))
}

pub(super) fn backup_id(input: &[u8]) -> Result<core::BackupId> {
    core::BackupId::from_bytes(array(input, "backup ID")?).map_err(runtime_error)
}

pub(super) fn restore_id(input: &[u8]) -> Result<core::RestoreId> {
    core::RestoreId::from_bytes(array(input, "restore ID")?).map_err(runtime_error)
}

pub(super) fn positive(n: f64, field: &str) -> Result<usize> {
    crate::input::positive_safe_usize(n)
        .ok_or_else(|| Error::from_reason(format!("{field} must be a positive safe integer")))
}

fn size(input: Option<f64>, default: usize, field: &str) -> Result<usize> {
    input.map_or(Ok(default), |n| positive(n, field))
}

fn bytes(input: Option<f64>, default: u64, field: &str) -> Result<u64> {
    input.map_or(Ok(default), |n| positive(n, field).map(|v| v as u64))
}

pub(super) fn optional_workspace(input: Option<f64>) -> Result<u64> {
    bytes(
        input,
        core::DEFAULT_BACKUP_WORKSPACE_BYTES,
        "maximumWorkspaceBytes",
    )
}

fn limit(input: Option<f64>, fallback: u32, field: &str) -> Result<u32> {
    input.map_or(Ok(fallback), |n| {
        u32::try_from(positive(n, field)?)
            .map_err(|_| Error::from_reason(format!("{field} exceeds uint32")))
    })
}

#[napi(object)]
#[derive(Default)]
pub struct BackupRepositoryOptions {
    pub max_key_bytes: Option<f64>,
    pub max_frame_bytes: Option<f64>,
    pub memory_bytes: Option<f64>,
    pub protection_domain: Option<Buffer>,
    pub key: Option<Buffer>,
}

impl BackupRepositoryOptions {
    pub(super) fn convert(self) -> Result<(core::Protection, core::BackupLimits, usize)> {
        let protection = match (self.protection_domain, self.key) {
            (None, None) => core::Protection::Plaintext,
            (Some(domain), Some(key)) => core::Protection::Encrypted {
                domain: core::ProtectionId::from_bytes(array(&domain, "protectionDomain")?)
                    .map_err(runtime_error)?,
                key: core::BackupKey::new(array(&key, "repository key")?),
            },
            _ => {
                return Err(Error::from_reason(
                    "encrypted backup repositories require both protectionDomain and key",
                ))
            }
        };
        let defaults = core::BackupLimits::default();
        let limits = core::BackupLimits::new(
            limit(self.max_key_bytes, defaults.max_key_bytes(), "maxKeyBytes")?,
            limit(
                self.max_frame_bytes,
                defaults.max_frame_bytes(),
                "maxFrameBytes",
            )?,
        )
        .map_err(runtime_error)?;
        Ok((
            protection,
            limits,
            size(
                self.memory_bytes,
                core::DEFAULT_BACKUP_MEMORY_BYTES,
                "memoryBytes",
            )?,
        ))
    }
}

#[napi(object)]
#[derive(Default)]
pub struct BackupOptions {
    pub parent: Option<Buffer>,
    pub maximum_encoded_bytes: Option<f64>,
    pub maximum_workspace_bytes: Option<f64>,
    pub memory_bytes: Option<f64>,
    pub allow_plaintext_export: Option<bool>,
}

impl BackupOptions {
    pub(super) fn convert(
        self,
        default_memory: usize,
    ) -> Result<(core::BackupOptions, core::StorageReadControl)> {
        let defaults = core::BackupOptions::default();
        Ok((
            core::BackupOptions {
                parent: self.parent.as_deref().map(backup_id).transpose()?,
                maximum_encoded_bytes: bytes(
                    self.maximum_encoded_bytes,
                    defaults.maximum_encoded_bytes,
                    "maximumEncodedBytes",
                )?,
                maximum_workspace_bytes: bytes(
                    self.maximum_workspace_bytes,
                    defaults.maximum_workspace_bytes,
                    "maximumWorkspaceBytes",
                )?,
                allow_plaintext_export: self
                    .allow_plaintext_export
                    .unwrap_or(defaults.allow_plaintext_export),
            },
            core::StorageReadControl::with_limit(size(
                self.memory_bytes,
                default_memory,
                "memoryBytes",
            )?),
        ))
    }
}

#[napi(object)]
#[derive(Default)]
pub struct RestoreOptions {
    pub maximum_workspace_bytes: Option<f64>,
    pub maximum_value_bytes: Option<f64>,
    pub retained_bytes: Option<f64>,
    pub memory_bytes: Option<f64>,
}

impl RestoreOptions {
    pub(super) fn convert(
        self,
        default_memory: usize,
    ) -> Result<(core::RestoreOptions, core::StorageReadControl)> {
        let defaults = core::RestoreOptions::default();
        Ok((
            core::RestoreOptions {
                maximum_workspace_bytes: bytes(
                    self.maximum_workspace_bytes,
                    defaults.maximum_workspace_bytes,
                    "maximumWorkspaceBytes",
                )?,
                maximum_value_bytes: size(
                    self.maximum_value_bytes,
                    defaults.maximum_value_bytes,
                    "maximumValueBytes",
                )?,
                retained_bytes: size(
                    self.retained_bytes,
                    defaults.retained_bytes,
                    "retainedBytes",
                )?,
            },
            core::StorageReadControl::with_limit(size(
                self.memory_bytes,
                default_memory,
                "memoryBytes",
            )?),
        ))
    }
}

#[napi(object)]
pub struct RestoreDestination {
    #[napi(ts_type = "\"memory\" | \"sqlite\" | \"sqlite-kv\"")]
    pub provider: String,
    pub directory: Option<String>,
    pub compression: Option<CompressionOptions>,
    pub key: Option<String>,
}

impl RestoreDestination {
    pub(super) fn convert(self) -> Result<core::RestoreDestination> {
        let mapping = match self.provider.as_str() {
            "memory" => {
                if self.directory.is_some() || self.compression.is_some() || self.key.is_some() {
                    return Err(Error::from_reason(
                        "memory restore does not take a directory, compression or key",
                    ));
                }
                return Ok(core::RestoreDestination::Memory);
            }
            "sqlite" => core::SQLiteBackupMapping::Native,
            "sqlite-kv" => core::SQLiteBackupMapping::KeyValue,
            _ => return Err(Error::from_reason("unknown restore destination provider")),
        };
        let path = self.directory.filter(|p| !p.is_empty()).ok_or_else(|| {
            Error::from_reason("a SQLite restore requires a fresh destination directory")
        })?;
        let mut target = core::SQLiteBackupTarget::new(mapping);
        target.compression = self
            .compression
            .map(|c| compression_options(Some(c)))
            .transpose()?;
        target.encryption_key = self.key.as_deref().map(core::StorageEncryptionKey::new);
        Ok(core::RestoreDestination::persistent(path, target))
    }
}
