//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Authorized, independently retained backup jobs over the installed provider.

mod functions;
mod memory;
mod restore;
mod validation;
pub(crate) use restore::RestoreBindings;
pub use restore::{
    RestoreConfiguration, RestoreDestination, RestoreJob, RestoreOptions, RestorePhase,
    RestoredEngine,
};

use crate::{Engine, SQLError};
use std::sync::Arc;

pub use uqa_storage::backup::{
    BackupCapabilities, BackupCapturePhase, BackupCaptureProgress, BackupCaptureReport,
    BackupCaptureStep, BackupCut, BackupError, BackupId, BackupKey, BackupLimits, BackupResult,
    BackupRoot, LocalRepository, PersistentBackupTarget, Protection, ProtectionId, RestoreId,
};
pub use uqa_storage::read_control::StorageReadControl;
pub use uqa_storage_sqlite::{SQLiteBackupMapping, SQLiteBackupTarget};

/// The repository owner implements progress, cancellation, verification and publication. The source Engine and provider may be dropped after construction.
pub type BackupJob<'a> =
    uqa_storage::backup::BackupImageCapture<'a, uqa_storage::backup::OwnedBackupImage>;

/// Explicit limits and lineage for one full or incremental export. Source and working memory are separately limited by the supplied read control.
#[derive(Clone, Copy, Debug)]
pub struct BackupOptions {
    pub parent: Option<BackupId>,
    pub maximum_encoded_bytes: u64,
    pub maximum_workspace_bytes: u64,
    pub allow_plaintext_export: bool,
}

impl BackupOptions {
    pub const fn new(maximum_encoded_bytes: u64, maximum_workspace_bytes: u64) -> Self {
        Self {
            parent: None,
            maximum_encoded_bytes,
            maximum_workspace_bytes,
            allow_plaintext_export: false,
        }
    }
}

impl Engine {
    /// Report the backup operations supported by this Engine’s storage provider.
    pub fn backup_capabilities(&self) -> BackupCapabilities {
        let memory = self.storage.memory_capture.is_some();
        let persistent = self
            .storage
            .provider
            .as_ref()
            .map_or(BackupCapabilities::UNSUPPORTED, |provider| {
                provider.backup_capabilities()
            });
        BackupCapabilities {
            export: memory || persistent.export,
            import: memory || persistent.import,
            snapshot_fingerprints: memory || persistent.snapshot_fingerprints,
        }
    }

    /// Start a committed export as the effective superuser. Admission follows the existing direct-query transaction boundary; no source SQL transaction or statement gate escapes into the job. Private and temporary state is excluded by the owning committed cut. Encrypted sources require an encrypted repository unless plaintext export is explicitly enabled. Capture/admission errors are SQL errors; subsequent job errors retain the repository's typed backup errors.
    pub fn begin_backup<'a>(
        &self,
        repository: &'a LocalRepository,
        options: BackupOptions,
        control: &StorageReadControl,
    ) -> Result<BackupJob<'a>, SQLError> {
        let control = control.with_temporary_limit(options.maximum_workspace_bytes);
        let authorize = |engine: &Self| -> Result<_, SQLError> {
            control.check().map_err(|error| {
                uqa_execution::storage_errors::storage_error("start backup", &error)
            })?;
            if !engine.current_user_is_superuser() {
                return Err(SQLError::Routine {
                    sqlstate: "42501".into(),
                    message: "database backup requires superuser authority".into(),
                });
            }
            if options.maximum_encoded_bytes == 0 || options.maximum_workspace_bytes == 0 {
                return Err(start_error(BackupError::Invalid("zero backup job limit")));
            }
            Ok(engine
                .extensions
                .functions
                .capture_declarations(control.memory())?)
        };
        let (source, catalogs, declarations) = if self.storage.memory_capture.is_some() {
            let (cut, declarations) = self.capture_committed_memory_query(&control, authorize)?;
            let catalogs =
                functions::Catalogs::memory(&cut).map_err(|error| start_error(error.into()))?;
            let image = memory::from_snapshot(self, cut, &control).map_err(start_error)?;
            (image, Some(catalogs), declarations)
        } else {
            self.with_direct_query_snapshot(
                true,
                |engine| {
                    let declarations = authorize(engine)?;
                    let provider = engine
                        .storage
                        .provider
                        .as_ref()
                        .filter(|provider| provider.backup_capabilities().export)
                        .ok_or_else(|| start_error(BackupError::Unsupported("Engine backup export")))?;
                    if provider.auxiliary_encryption_key().is_some()
                        && repository.protection_domain().is_none()
                        && !options.allow_plaintext_export
                    {
                        return Err(start_error(BackupError::Invalid(
                            "encrypted source requires a protected repository or explicit plaintext export",
                        )));
                    }
                    let image = provider.capture_backup_image(&control).map_err(start_error)?;
                    let limit = control.memory().limit();
                    let limits = uqa_storage::catalog::input::CatalogInputLimits {
                        maximum_key_bytes: limit,
                        maximum_value_bytes: limit,
                    };
                    let catalogs = functions::Catalogs::provider(&image, limits, &control).map_err(start_error)?;
                    Ok((image, catalogs, declarations))
                },
                std::convert::identity,
            )?
        };
        let source = if let Some(catalogs) = catalogs {
            let references = functions::References::new(catalogs, declarations, &control)
                .map_err(start_error)?;
            source
                .with_function_references(references, &control)
                .map_err(start_error)?
        } else {
            source
        };
        repository
            .begin_image_backup(
                source,
                options.parent,
                options.maximum_encoded_bytes,
                options.maximum_workspace_bytes,
                &control,
            )
            .map_err(start_error)
    }
}

fn start_error(error: BackupError) -> SQLError {
    match error {
        BackupError::Memory(error) => error.into(),
        BackupError::Cancelled(error) => error.into(),
        BackupError::Storage(error) => {
            uqa_execution::storage_errors::storage_error("start backup", &error)
        }
        error => SQLError::Host(Arc::new(error)),
    }
}
