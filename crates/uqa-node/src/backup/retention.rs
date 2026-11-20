//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use napi::bindgen_prelude::{AsyncTask, BigInt, Buffer};
use napi::{Error, Result};
use napi_derive::napi;
use uqa_engine::backup::StorageReadControl;

use super::options::{backup_id, optional_workspace, positive, BackupOptions};
use super::{work, BackupRepository, BackupRoot, BackupTask};
use crate::input::runtime_error;

#[napi(object)]
pub struct BackupCollectionReport {
    pub completed_backups: BigInt,
    pub prepared_backups: BigInt,
    pub active_operations: BigInt,
    pub referenced_frame_bytes: BigInt,
    pub retained_object_bytes: BigInt,
    pub unreferenced_object_bytes: BigInt,
    pub removed_objects: BigInt,
    pub removed_object_bytes: BigInt,
    pub removed_staging_files: BigInt,
    pub removed_staging_bytes: BigInt,
    pub remaining_files: BigInt,
    pub workspace_peak_bytes: BigInt,
}

#[napi]
impl BackupRepository {
    #[napi(ts_return_type = "Promise<BackupCollectionReport>")]
    pub fn collect_garbage(
        &self,
        maximum_files: f64,
        maximum_workspace_bytes: Option<f64>,
    ) -> Result<AsyncTask<BackupTask<BackupCollectionReport>>> {
        let files = positive(maximum_files, "maximumFiles")?;
        let workspace = optional_workspace(maximum_workspace_bytes)?;
        let repo = self.owner()?;
        let control = StorageReadControl::with_limit(self.memory_bytes);
        Ok(work(move || {
            let r = repo
                .collect_garbage(files, workspace, &control)
                .map_err(runtime_error)?;
            Ok(BackupCollectionReport {
                completed_backups: r.completed_backups.into(),
                prepared_backups: r.prepared_backups.into(),
                active_operations: r.active_operations.into(),
                referenced_frame_bytes: r.referenced_frame_bytes.into(),
                retained_object_bytes: r.retained_object_bytes.into(),
                unreferenced_object_bytes: r.unreferenced_object_bytes.into(),
                removed_objects: r.removed_objects.into(),
                removed_object_bytes: r.removed_object_bytes.into(),
                removed_staging_files: r.removed_staging_files.into(),
                removed_staging_bytes: r.removed_staging_bytes.into(),
                remaining_files: r.remaining_files.into(),
                workspace_peak_bytes: r.workspace_peak_bytes.into(),
            })
        }))
    }

    #[napi(ts_return_type = "Promise<BackupRoot>")]
    pub fn copy_to(
        &self,
        id: Buffer,
        destination: &BackupRepository,
        options: Option<BackupOptions>,
    ) -> Result<AsyncTask<BackupTask<BackupRoot>>> {
        let id = backup_id(&id)?;
        let options = options.unwrap_or_default();
        if options.parent.is_some() {
            return Err(Error::from_reason(
                "standalone archive copy does not take a parent",
            ));
        }
        let (options, control) = options.convert(self.memory_bytes)?;
        let source = self.owner()?;
        let destination = destination.owner()?;
        Ok(work(move || {
            source
                .copy_backup_to(
                    id,
                    &destination,
                    options.maximum_encoded_bytes,
                    options.maximum_workspace_bytes,
                    options.allow_plaintext_export,
                    &control,
                )
                .map(BackupRoot::from)
                .map_err(runtime_error)
        }))
    }

    #[napi(ts_return_type = "Promise<BackupRoot>")]
    pub fn resume_publication(
        &self,
        id: Buffer,
        maximum_workspace_bytes: Option<f64>,
    ) -> Result<AsyncTask<BackupTask<BackupRoot>>> {
        let id = backup_id(&id)?;
        let workspace = optional_workspace(maximum_workspace_bytes)?;
        let repo = self.owner()?;
        let control = StorageReadControl::with_limit(self.memory_bytes);
        Ok(work(move || {
            repo.resume_publication(id, workspace, &control)
                .and_then(|mut op| op.publish(&control))
                .map(BackupRoot::from)
                .map_err(runtime_error)
        }))
    }
}
