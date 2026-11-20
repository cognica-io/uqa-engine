//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Independently owned handles for callers whose repository and job lifetimes are unrelated.

use super::{
    BackupCaptureProgress, BackupCaptureReport, BackupCaptureStep, BackupId, BackupJob,
    BackupOptions, BackupResult, BackupRoot, LocalRepository, StorageReadControl,
};
use crate::{Engine, SQLError};
use std::sync::Arc;

self_cell::self_cell!(
    struct CaptureCell {
        owner: Arc<LocalRepository>,
        #[not_covariant]
        dependent: BackupJob,
    }
);

/// A capture job that retains the repository until its last operation and lease have been dropped.
pub struct OwnedBackupJob(CaptureCell);

impl OwnedBackupJob {
    pub fn begin(
        engine: &Engine,
        repository: Arc<LocalRepository>,
        options: BackupOptions,
        control: &StorageReadControl,
    ) -> Result<Self, SQLError> {
        CaptureCell::try_new(repository, |repo| {
            engine.begin_backup(repo, options, control)
        })
        .map(Self)
    }

    pub fn id(&self) -> BackupId {
        self.0.with_dependent(|_, job| job.id())
    }

    pub fn progress(&self) -> BackupCaptureProgress {
        self.0.with_dependent(|_, job| job.progress())
    }

    pub fn report(&self) -> Option<BackupCaptureReport> {
        self.0.with_dependent(|_, job| job.report())
    }

    pub fn advance(
        &mut self,
        maximum_positions: usize,
        maximum_bytes: usize,
    ) -> BackupResult<BackupCaptureStep> {
        self.0
            .with_dependent_mut(|_, job| job.advance(maximum_positions, maximum_bytes))
    }

    pub fn publish(&mut self) -> BackupResult<BackupRoot> {
        self.0.with_dependent_mut(|_, job| job.publish())
    }

    pub fn finish(&mut self) -> BackupResult<BackupRoot> {
        self.0.with_dependent_mut(|_, job| job.finish())
    }

    pub fn cancel(&mut self) -> BackupResult<Option<BackupRoot>> {
        self.0.with_dependent_mut(|_, job| job.cancel())
    }
}

mod restore;
pub use restore::OwnedRestoreJob;

#[cfg(test)]
mod tests;
