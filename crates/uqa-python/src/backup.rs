//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native local repositories and independently owned Python backup and restore jobs.

mod jobs;
mod options;
mod repository;

use crate::{runtime_error, PyEngine};
use jobs::{PyBackupJob, PyRestoreJob};
use options::{PyBackupOptions, PyRestoreDestination, PyRestoreOptions};
use pyo3::{
    exceptions::{PyRuntimeError, PyValueError},
    prelude::*,
    types::PyBytes,
};
use repository::{PyBackupListing, PyBackupRepository};
use std::sync::Mutex;
use uqa_engine::backup::{
    BackupId, BackupResult, OwnedBackupJob, OwnedRestoreJob, RestoreDestination, RestoreId,
};

pub(super) fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<PyBackupRepository>()?;
    module.add_class::<PyBackupListing>()?;
    module.add_class::<PyBackupOptions>()?;
    module.add_class::<PyBackupJob>()?;
    module.add_class::<PyRestoreDestination>()?;
    module.add_class::<PyRestoreOptions>()?;
    module.add_class::<PyRestoreJob>()
}

/// Release native cursors and their leases on explicit close; serialize active operations without holding the GIL.
struct Handle<T>(Mutex<Option<T>>);

impl<T: Send> Handle<T> {
    fn new(value: T) -> Self {
        Self(Mutex::new(Some(value)))
    }

    fn run<R: Send>(
        &self,
        py: Python<'_>,
        operation: impl FnOnce(&mut T) -> BackupResult<R> + Send,
    ) -> PyResult<R> {
        py.detach(|| {
            let mut state = self.0.lock().map_err(runtime_error)?;
            let owner = state
                .as_mut()
                .ok_or_else(|| PyRuntimeError::new_err("backup handle is closed"))?;
            operation(owner).map_err(runtime_error)
        })
    }

    fn close(&self, py: Python<'_>) -> PyResult<()> {
        py.detach(|| {
            let owner = self.0.lock().map_err(runtime_error)?.take();
            drop(owner);
            Ok(())
        })
    }
}

fn fixed_bytes<const N: usize>(value: &[u8], field: &str) -> PyResult<[u8; N]> {
    value
        .try_into()
        .map_err(|_| PyValueError::new_err(format!("{field} must contain exactly {N} bytes")))
}

fn backup_identity<'a, T>(value: &'a [u8], name: &str) -> PyResult<T>
where
    T: TryFrom<&'a [u8], Error = uqa_engine::backup::ParseBackupIdentityError>,
{
    use uqa_engine::backup::ParseBackupIdentityError;
    T::try_from(value).map_err(|error| match error {
        ParseBackupIdentityError::Length => {
            PyValueError::new_err(format!("{name} must contain exactly 16 bytes"))
        }
        ParseBackupIdentityError::Invalid(error) => runtime_error(error),
    })
}

fn backup_id(value: &[u8]) -> PyResult<BackupId> {
    backup_identity(value, "backup ID")
}

fn restore_id(value: &[u8]) -> PyResult<RestoreId> {
    backup_identity(value, "restore ID")
}

fn id_bytes(py: Python<'_>, id: [u8; 16]) -> Py<PyBytes> {
    PyBytes::new(py, &id).unbind()
}

#[pymethods]
impl PyEngine {
    /// Capture this session's committed database state under an optional parent backup.
    #[pyo3(signature = (repository, *, parent=None, options=None))]
    fn begin_backup(
        &self,
        py: Python<'_>,
        repository: &PyBackupRepository,
        parent: Option<&[u8]>,
        options: Option<&PyBackupOptions>,
    ) -> PyResult<PyBackupJob> {
        let engine = self.inner()?;
        let mut options = options.copied().unwrap_or_default();
        options.capture.parent = parent.map(backup_id).transpose()?;
        let control = repository.control(options.memory_bytes);
        let repository = repository.retained(py)?;
        py.detach(|| OwnedBackupJob::begin(engine, repository, options.capture, &control))
            .map(PyBackupJob::new)
            .map_err(runtime_error)
    }

    /// Restore into a new writable database. A missing destination selects memory.
    #[pyo3(signature = (repository, backup, *, destination=None, options=None))]
    fn begin_restore(
        &self,
        py: Python<'_>,
        repository: &PyBackupRepository,
        backup: &[u8],
        destination: Option<&PyRestoreDestination>,
        options: Option<&PyRestoreOptions>,
    ) -> PyResult<PyRestoreJob> {
        let engine = self.inner()?;
        let backup = backup_id(backup)?;
        let destination = destination.map_or(RestoreDestination::Memory, |d| d.0.clone());
        let options = options.copied().unwrap_or_default();
        let control = repository.control(options.memory_bytes);
        let repository = repository.retained(py)?;
        py.detach(|| {
            OwnedRestoreJob::begin(
                engine,
                repository,
                backup,
                destination,
                options.restore.with_runtime(engine),
                &control,
            )
        })
        .map(PyRestoreJob::new)
        .map_err(runtime_error)
    }

    /// Recover the identified durable restore in its original destination directory.
    #[pyo3(signature = (repository, restore, *, destination, options=None))]
    fn resume_restore(
        &self,
        py: Python<'_>,
        repository: &PyBackupRepository,
        restore: &[u8],
        destination: &PyRestoreDestination,
        options: Option<&PyRestoreOptions>,
    ) -> PyResult<PyRestoreJob> {
        let engine = self.inner()?;
        let restore = restore_id(restore)?;
        let options = options.copied().unwrap_or_default();
        let control = repository.control(options.memory_bytes);
        let repository = repository.retained(py)?;
        py.detach(|| {
            OwnedRestoreJob::resume(
                engine,
                repository,
                restore,
                destination.0.clone(),
                options.restore.with_runtime(engine),
                &control,
            )
        })
        .map(PyRestoreJob::new)
        .map_err(runtime_error)
    }

    #[pyo3(signature = (repository, restore, *, destination, options=None))]
    fn cancel_restore(
        &self,
        py: Python<'_>,
        repository: &PyBackupRepository,
        restore: &[u8],
        destination: &PyRestoreDestination,
        options: Option<&PyRestoreOptions>,
    ) -> PyResult<Option<Self>> {
        let engine = self.inner()?;
        let restore = restore_id(restore)?;
        let options = options.copied().unwrap_or_default();
        let control = repository.control(options.memory_bytes);
        let repository = repository.retained(py)?;
        py.detach(|| {
            engine.cancel_restore(
                &repository,
                restore,
                destination.0.clone(),
                options.restore.with_runtime(engine),
                &control,
            )
        })
        .map(|result| result.map(jobs::restored_engine))
        .map_err(runtime_error)
    }
}
