//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A recovered prepared database retains its private directory and exclusive request until the current runtime resources have been checked.

use super::{
    directory, native, BackupError, BackupRestoreOperation, BackupRestoreWorkStep, BackupResult,
    DatabaseRestore, Dir, Path, RestoredDatabase, State, StorageReadControl,
};

pub enum PreparedRestoreStep<'a, W> {
    Pending(PreparedRestoreValidation<'a, W>),
    Complete(BackupRestoreOperation<'a>),
}

pub struct PreparedRestoreValidation<'a, W> {
    // Provider handles close before the private pathname and exclusive operation are released.
    work: W,
    path: native::FilePath,
    holder: Dir,
    operation: BackupRestoreOperation<'a>,
}

impl<'a> BackupRestoreOperation<'a> {
    /// Start validation of the original prepared directory against current process-local resources without requiring the source archive. An already published request proceeds directly to its existing destination. The opener must keep its provider handles in the returned work.
    pub fn begin_prepared_validation<W>(
        self,
        open: impl FnOnce(&Path, DatabaseRestore, &StorageReadControl) -> BackupResult<W>,
    ) -> BackupResult<PreparedRestoreStep<'a, W>> {
        match self.state {
            State::Complete => return Ok(PreparedRestoreStep::Complete(self)),
            State::Prepared => (),
            _ => return Err(BackupError::Finished),
        }
        self.control.check()?;
        let (holder, image) = self.stage()?;
        if !self.prepared_marker(&image)? {
            return Err(BackupError::Integrity);
        }
        let path = native::FilePath::new(&holder, "image".as_ref(), &self.control)?;
        let work = open(path.as_path(), self.request(), &self.control)?;
        let validation = PreparedRestoreValidation {
            work,
            path,
            holder,
            operation: self,
        };
        validation.verify()?;
        Ok(PreparedRestoreStep::Pending(validation))
    }
}

impl<'a, W> PreparedRestoreValidation<'a, W> {
    fn verify(&self) -> BackupResult<()> {
        self.operation.control.check()?;
        self.path.verify(&self.holder)?;
        let image = directory::child(&self.holder, "image".as_ref())?;
        if native::identity(&image)? != self.operation.target.image {
            return Err(BackupError::Integrity);
        }
        Ok(())
    }

    /// Advance one work window. A successful completion closes provider handles before returning the request for publication. Errors and unwinding release both the handles and admission.
    pub fn advance(
        mut self,
        validate: impl FnOnce(&mut W) -> BackupResult<BackupRestoreWorkStep>,
    ) -> BackupResult<PreparedRestoreStep<'a, W>> {
        self.verify()?;
        let step = validate(&mut self.work)?;
        self.verify()?;
        match step {
            BackupRestoreWorkStep::Pending => Ok(PreparedRestoreStep::Pending(self)),
            BackupRestoreWorkStep::Complete => {
                drop(self.work);
                Ok(PreparedRestoreStep::Complete(self.operation))
            }
        }
    }

    /// Close private validation handles before retiring the durable request.
    pub fn cancel(mut self) -> BackupResult<Option<RestoredDatabase>> {
        drop(self.work);
        self.operation.cancel()
    }
}
