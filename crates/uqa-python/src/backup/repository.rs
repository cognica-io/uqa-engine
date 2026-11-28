//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{backup_id, fixed_bytes, id_bytes, Handle, PyBackupOptions};
use crate::runtime_error;
use pyo3::{
    exceptions::PyValueError,
    prelude::*,
    types::{PyBytes, PyDict},
};
use std::{path::PathBuf, sync::Arc};
use uqa_engine::backup::{
    BackupKey, BackupLimits, BackupListing, LocalRepository, Protection, StorageReadControl,
    DEFAULT_BACKUP_MEMORY_BYTES, DEFAULT_BACKUP_WORKSPACE_BYTES,
};

fn protection(domain: Option<&[u8]>, key: Option<&[u8]>) -> PyResult<Protection> {
    match (domain, key) {
        (None, None) => Ok(Protection::Plaintext),
        (Some(domain), Some(key)) => Ok(Protection::Encrypted {
            domain: super::backup_identity(domain, "protection domain")?,
            key: BackupKey::new(fixed_bytes(key, "repository key")?),
        }),
        _ => Err(PyValueError::new_err(
            "encrypted repositories require both a protection domain and a key",
        )),
    }
}

#[pyclass(name = "BackupRepository", module = "uqa._uqa", frozen)]
pub(super) struct PyBackupRepository {
    inner: Handle<Arc<LocalRepository>>,
    memory_bytes: usize,
}

impl PyBackupRepository {
    pub(super) fn retained(&self, py: Python<'_>) -> PyResult<Arc<LocalRepository>> {
        self.inner.run(py, |owner| Ok(Arc::clone(owner)))
    }

    pub(super) fn control(&self, memory_bytes: Option<usize>) -> StorageReadControl {
        StorageReadControl::with_limit(memory_bytes.unwrap_or(self.memory_bytes))
    }
}

#[pymethods]
impl PyBackupRepository {
    /// Create a new local repository. Existing repositories must be opened separately.
    #[staticmethod]
    #[pyo3(signature = (path, *, protection_domain=None, key=None, max_key_bytes=None, max_frame_bytes=None, memory_bytes=DEFAULT_BACKUP_MEMORY_BYTES))]
    fn create(
        py: Python<'_>,
        path: PathBuf,
        protection_domain: Option<&[u8]>,
        key: Option<&[u8]>,
        max_key_bytes: Option<u32>,
        max_frame_bytes: Option<u32>,
        memory_bytes: usize,
    ) -> PyResult<Self> {
        let protection = protection(protection_domain, key)?;
        let limits = limits(max_key_bytes, max_frame_bytes)?;
        let control = StorageReadControl::with_limit(memory_bytes);
        let repository = py
            .detach(|| LocalRepository::create(path, protection, limits, &control))
            .map_err(runtime_error)?;
        Ok(Self {
            inner: Handle::new(Arc::new(repository)),
            memory_bytes,
        })
    }

    #[staticmethod]
    #[pyo3(signature = (path, *, protection_domain=None, key=None, max_key_bytes=None, max_frame_bytes=None, memory_bytes=DEFAULT_BACKUP_MEMORY_BYTES))]
    fn open(
        py: Python<'_>,
        path: PathBuf,
        protection_domain: Option<&[u8]>,
        key: Option<&[u8]>,
        max_key_bytes: Option<u32>,
        max_frame_bytes: Option<u32>,
        memory_bytes: usize,
    ) -> PyResult<Self> {
        let protection = protection(protection_domain, key)?;
        let limits = limits(max_key_bytes, max_frame_bytes)?;
        let control = StorageReadControl::with_limit(memory_bytes);
        let repository = py
            .detach(|| LocalRepository::open(path, protection, limits, &control))
            .map_err(runtime_error)?;
        Ok(Self {
            inner: Handle::new(Arc::new(repository)),
            memory_bytes,
        })
    }

    fn close(&self, py: Python<'_>) -> PyResult<()> {
        self.inner.close(py)
    }

    #[getter]
    fn id(&self, py: Python<'_>) -> PyResult<Py<PyBytes>> {
        Ok(id_bytes(py, self.retained(py)?.id().as_bytes()))
    }

    #[getter]
    fn protection_domain(&self, py: Python<'_>) -> PyResult<Option<Py<PyBytes>>> {
        Ok(self
            .retained(py)?
            .protection_domain()
            .map(|domain| id_bytes(py, domain.as_bytes())))
    }

    #[pyo3(signature = (*, maximum_workspace_bytes=DEFAULT_BACKUP_WORKSPACE_BYTES))]
    fn list(&self, py: Python<'_>, maximum_workspace_bytes: u64) -> PyResult<PyBackupListing> {
        let repository = self.retained(py)?;
        py.detach(|| repository.list_backups(maximum_workspace_bytes, &self.control(None)))
            .map(|listing| PyBackupListing(Handle::new(listing)))
            .map_err(runtime_error)
    }

    /// Read the parent named by this completed backup, even if that parent was removed.
    fn parent(&self, py: Python<'_>, backup: &[u8]) -> PyResult<Option<Py<PyBytes>>> {
        let repository = self.retained(py)?;
        let id = backup_id(backup)?;
        let parent = py
            .detach(|| {
                repository
                    .open_backup(id, &self.control(None))
                    .map(|lease| lease.root().parent())
            })
            .map_err(runtime_error)?;
        Ok(parent.map(|id| id_bytes(py, id.as_bytes())))
    }

    #[pyo3(signature = (backup, *, maximum_workspace_bytes=DEFAULT_BACKUP_WORKSPACE_BYTES))]
    fn verify(&self, py: Python<'_>, backup: &[u8], maximum_workspace_bytes: u64) -> PyResult<()> {
        let repository = self.retained(py)?;
        let id = backup_id(backup)?;
        let control = self.control(None);
        py.detach(|| {
            repository
                .open_backup(id, &control)?
                .verify(maximum_workspace_bytes, &control)
        })
        .map_err(runtime_error)
    }

    /// Publish a previously prepared capture after its original job was dropped.
    #[pyo3(signature = (backup, *, maximum_workspace_bytes=DEFAULT_BACKUP_WORKSPACE_BYTES))]
    fn publish_prepared(
        &self,
        py: Python<'_>,
        backup: &[u8],
        maximum_workspace_bytes: u64,
    ) -> PyResult<Py<PyBytes>> {
        let repository = self.retained(py)?;
        let id = backup_id(backup)?;
        let control = self.control(None);
        let root = py
            .detach(|| {
                repository
                    .resume_publication(id, maximum_workspace_bytes, &control)?
                    .publish(&control)
            })
            .map_err(runtime_error)?;
        Ok(id_bytes(py, root.backup().as_bytes()))
    }

    /// Copy a verified root into a separately protected, independently restorable repository.
    #[pyo3(signature = (backup, destination, *, options=None))]
    fn copy_to(
        &self,
        py: Python<'_>,
        backup: &[u8],
        destination: &PyBackupRepository,
        options: Option<&PyBackupOptions>,
    ) -> PyResult<Py<PyBytes>> {
        let source = self.retained(py)?;
        let destination = destination.retained(py)?;
        let id = backup_id(backup)?;
        let options = options.copied().unwrap_or_default();
        let control = self.control(options.memory_bytes);
        let root = py
            .detach(|| {
                source.copy_backup_to(
                    id,
                    &destination,
                    options.capture.maximum_encoded_bytes,
                    options.capture.maximum_workspace_bytes,
                    options.capture.allow_plaintext_export,
                    &control,
                )
            })
            .map_err(runtime_error)?;
        Ok(id_bytes(py, root.backup().as_bytes()))
    }

    /// Remove at most the requested number of unreferenced files and report the remaining work.
    #[pyo3(signature = (maximum_files, *, maximum_workspace_bytes=DEFAULT_BACKUP_WORKSPACE_BYTES))]
    fn collect_garbage(
        &self,
        py: Python<'_>,
        maximum_files: usize,
        maximum_workspace_bytes: u64,
    ) -> PyResult<Py<PyDict>> {
        let repository = self.retained(py)?;
        let control = self.control(None);
        let report = py
            .detach(|| repository.collect_garbage(maximum_files, maximum_workspace_bytes, &control))
            .map_err(runtime_error)?;
        let counts = PyDict::new(py);
        for (name, value) in [
            ("completed_backups", report.completed_backups),
            ("prepared_backups", report.prepared_backups),
            ("active_operations", report.active_operations),
            ("referenced_frame_bytes", report.referenced_frame_bytes),
            ("retained_object_bytes", report.retained_object_bytes),
            (
                "unreferenced_object_bytes",
                report.unreferenced_object_bytes,
            ),
            ("removed_objects", report.removed_objects),
            ("removed_object_bytes", report.removed_object_bytes),
            ("removed_staging_files", report.removed_staging_files),
            ("removed_staging_bytes", report.removed_staging_bytes),
            ("remaining_files", report.remaining_files),
            ("workspace_peak_bytes", report.workspace_peak_bytes),
        ] {
            counts.set_item(name, value)?;
        }
        Ok(counts.unbind())
    }

    fn remove(&self, py: Python<'_>, backup: &[u8]) -> PyResult<()> {
        let repository = self.retained(py)?;
        let id = backup_id(backup)?;
        py.detach(|| repository.remove_backup(id, &self.control(None)))
            .map_err(runtime_error)
    }
}

fn limits(keys: Option<u32>, frames: Option<u32>) -> PyResult<BackupLimits> {
    let defaults = BackupLimits::default();
    BackupLimits::new(
        keys.unwrap_or(defaults.max_key_bytes()),
        frames.unwrap_or(defaults.max_frame_bytes()),
    )
    .map_err(runtime_error)
}

#[pyclass(name = "BackupListing", module = "uqa._uqa", frozen)]
pub(super) struct PyBackupListing(Handle<BackupListing>);

#[pymethods]
impl PyBackupListing {
    fn close(&self, py: Python<'_>) -> PyResult<()> {
        self.0.close(py)
    }

    fn __iter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    fn __next__(&self, py: Python<'_>) -> PyResult<Option<Py<PyBytes>>> {
        let next = self.0.run(py, BackupListing::next_backup)?;
        Ok(next.map(|root| id_bytes(py, root.backup().as_bytes())))
    }
}
