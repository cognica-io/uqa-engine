//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scheduled imports share durable admission, recovery and publication with consuming importers.

use super::{
    native, BackupError, BackupLease, BackupRestoreOperation, BackupResult, DatabaseRestore, Dir,
    Path, RestoreDirectory, RestoreId, RestoredDatabase, State, StorageReadControl,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackupRestoreWorkStep {
    Pending,
    Complete,
}

/// One installed provider's import, logical validation and history transition. Each advance must respect that owner's admitted record, byte and workspace limits. Complete certifies the same conditions as successful `BackupRestoreImporter::import`; an incomplete target must never become usable. Drop closes every retained provider handle, including after an error or unwind.
pub trait BackupRestoreWork {
    fn advance(&mut self) -> BackupResult<BackupRestoreWorkStep>;
}

/// Start or recover a provider-owned continuation. Constructors only admit the private target and bounded initial state; traversal belongs to `advance`. Recovery must resolve the exact persisted request without replaying an import over existing data. The root lease can move directly into an image reader; no sibling borrowed lease is required.
pub trait BackupRestoreFactory<'a>: Sized {
    type Work: BackupRestoreWork;

    fn import(
        self,
        directory: &RestoreDirectory<'_>,
        backup: BackupLease<'a>,
        request: DatabaseRestore,
        control: &StorageReadControl,
    ) -> BackupResult<Self::Work>;

    fn recover(
        self,
        directory: &RestoreDirectory<'_>,
        backup: BackupLease<'a>,
        request: DatabaseRestore,
        control: &StorageReadControl,
    ) -> BackupResult<Self::Work>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackupRestoreStep {
    Pending,
    Prepared,
    Complete,
}

/// Own a provider continuation and its exclusive repository operation. `Prepared` requires all importer handles to have closed before the durable marker is installed; publication remains explicit. Failure or unwinding drops the continuation at that step, leaving the started intent for exact recovery. Dropping the job releases admission without cancelling the durable intent.
pub struct BackupRestoreJob<'a, W: BackupRestoreWork> {
    // Provider files close before operation admission is released, including during unwinding.
    work: Option<W>,
    operation: BackupRestoreOperation<'a>,
}

impl<'a> BackupRestoreOperation<'a> {
    pub fn into_job<F: BackupRestoreFactory<'a>>(
        mut self,
        factory: F,
    ) -> BackupResult<BackupRestoreJob<'a, F::Work>> {
        let work = match self.admit_import()? {
            None => None,
            Some((image, fresh)) => {
                let directory = RestoreDirectory { directory: &image };
                // Keep the operation's original lease through logical validation and preparation;
                // an independently owned reader can finish and retire its own lease earlier.
                let backup = self
                    .repository
                    .open_backup(self.target.intent.backup, &self.control)?;
                Some(if fresh {
                    factory.import(&directory, backup, self.request(), &self.control)?
                } else {
                    factory.recover(&directory, backup, self.request(), &self.control)?
                })
            }
        };
        Ok(BackupRestoreJob {
            work,
            operation: self,
        })
    }
}

impl<W: BackupRestoreWork> BackupRestoreJob<'_, W> {
    pub const fn id(&self) -> RestoreId {
        self.operation.id()
    }

    pub const fn request(&self) -> DatabaseRestore {
        self.operation.request()
    }

    pub fn completed(&self) -> Option<RestoredDatabase> {
        self.operation.completed()
    }

    /// Perform one owner work step. Complete provider work is dropped before preparing publication. A failed step cannot execute again; close the job and resume the durable operation to enter recovery.
    pub fn advance(&mut self) -> BackupResult<BackupRestoreStep> {
        match self.operation.state {
            State::Complete => return Ok(BackupRestoreStep::Complete),
            State::Prepared => return Ok(BackupRestoreStep::Prepared),
            State::Cancelling | State::Cancelled => return Err(BackupError::Finished),
            State::New | State::Started => (),
        }
        // Taking the owner makes both errors and unwinding terminal without a caller repair path.
        let mut work = self.work.take().ok_or(BackupError::Finished)?;
        self.operation.control.cancellation().check()?;
        if work.advance()? == BackupRestoreWorkStep::Pending {
            self.work = Some(work);
            return Ok(BackupRestoreStep::Pending);
        }
        drop(work);
        let (_, image) = self.operation.stage()?;
        self.operation.finish_import(&image)?;
        Ok(BackupRestoreStep::Prepared)
    }

    pub fn publish(&mut self) -> BackupResult<RestoredDatabase> {
        self.operation.publish()
    }

    pub fn cancel(&mut self) -> BackupResult<Option<RestoredDatabase>> {
        self.work.take();
        self.operation.cancel()
    }
}

struct FileBinding {
    path: native::FilePath,
    directory: Dir,
}

impl FileBinding {
    fn verify(&self) -> BackupResult<()> {
        self.path.verify(&self.directory)?;
        let name = self
            .path
            .as_path()
            .file_name()
            .expect("admitted child path");
        if !self.directory.symlink_metadata(name)?.is_file() {
            return Err(BackupError::Invalid(
                "provider replaced its file with a nonregular entry",
            ));
        }
        Ok(())
    }
}

struct FileWork<W> {
    // Physical owners retire before the pinned directory and pathname allowance.
    work: W,
    binding: FileBinding,
}

/// Keep an installed filename-based provider attached to its private directory across work steps. Directory movement or a nonregular file makes the continuation terminal; recovery resolves its current pinned location. Provider work must keep all file access inside this directory and close its handles when dropped.
pub struct RestoreFileWork<W: BackupRestoreWork> {
    active: Option<FileWork<W>>,
    complete: bool,
}

impl RestoreDirectory<'_> {
    /// Open a continuation for an existing private regular file. The constructor may open provider handles but cannot retain the borrowed pathname; its provider may retain its own opening path under this wrapper's directory checks. No process-wide working directory changes occur.
    pub fn begin_file_work<W: BackupRestoreWork>(
        &self,
        name: &str,
        control: &StorageReadControl,
        open: impl FnOnce(&Path) -> BackupResult<W>,
    ) -> BackupResult<RestoreFileWork<W>> {
        Self::validate_name(name)?;
        if !self.directory.symlink_metadata(name)?.is_file() {
            return Err(BackupError::Invalid(
                "provider path requires a private regular file",
            ));
        }
        let binding = FileBinding {
            path: native::FilePath::new(self.directory, name.as_ref(), control)?,
            directory: self.directory.try_clone()?,
        };
        let active = FileWork {
            work: open(binding.path.as_path())?,
            binding,
        };
        active.binding.verify()?;
        Ok(RestoreFileWork {
            active: Some(active),
            complete: false,
        })
    }
}

impl<W: BackupRestoreWork> BackupRestoreWork for RestoreFileWork<W> {
    fn advance(&mut self) -> BackupResult<BackupRestoreWorkStep> {
        if self.complete {
            return Ok(BackupRestoreWorkStep::Complete);
        }
        let mut active = self.active.take().ok_or(BackupError::Finished)?;
        active.binding.verify()?;
        let step = active.work.advance()?;
        active.binding.verify()?;
        if step == BackupRestoreWorkStep::Complete {
            drop(active);
            self.complete = true;
        } else {
            self.active = Some(active);
        }
        Ok(step)
    }
}
