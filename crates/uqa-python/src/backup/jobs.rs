//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{id_bytes, Handle};
use crate::PyEngine;
use pyo3::{
    prelude::*,
    types::{PyBytes, PyDict},
};
use uqa_engine::backup::{
    BackupCaptureStep, OwnedBackupJob, OwnedRestoreJob, RestorePhase, RestoredEngine,
};

#[pyclass(name = "BackupJob", module = "uqa._uqa", frozen)]
pub(super) struct PyBackupJob(Handle<OwnedBackupJob>);

impl PyBackupJob {
    pub(super) fn new(job: OwnedBackupJob) -> Self {
        Self(Handle::new(job))
    }
}

#[pymethods]
impl PyBackupJob {
    fn close(&self, py: Python<'_>) -> PyResult<()> {
        self.0.close(py)
    }

    #[getter]
    fn id(&self, py: Python<'_>) -> PyResult<Py<PyBytes>> {
        let id = self.0.run(py, |job| Ok(job.id()))?;
        Ok(id_bytes(py, id.as_bytes()))
    }

    #[getter]
    fn phase(&self, py: Python<'_>) -> PyResult<&'static str> {
        self.0.run(py, |job| Ok(job.progress().phase.as_str()))
    }

    /// Actual source, image reuse, output and verification counters for this capture.
    #[getter]
    fn progress(&self, py: Python<'_>) -> PyResult<Py<PyDict>> {
        let progress = self.0.run(py, |job| Ok(job.progress()))?;
        let source = progress.source;
        let image = progress.image;
        let counts = PyDict::new(py);
        for (name, value) in [
            ("visited_positions", source.visited_positions),
            ("copied_payload_bytes", source.copied_payload_bytes),
            ("metadata_read_bytes", source.metadata_read_bytes),
            ("excluded_units", source.excluded_units),
            ("parent_units", image.parent_units),
            ("hashed_payload_bytes", image.hashed_payload_bytes),
            ("certified_reused_units", image.certified_reused_units),
            ("certified_reused_bytes", image.certified_reused_bytes),
            ("reused_units", image.reused_units),
            ("reused_payload_bytes", image.reused_payload_bytes),
            ("written_units", image.written_units),
            ("written_payload_bytes", image.written_payload_bytes),
            ("written_object_bytes", image.written_object_bytes),
            ("verified_object_bytes", progress.verified_object_bytes),
        ] {
            counts.set_item(name, value)?;
        }
        Ok(counts.unbind())
    }

    /// The verified inventory summary once the job has prepared its root.
    #[getter]
    fn report(&self, py: Python<'_>) -> PyResult<Option<Py<PyDict>>> {
        let Some(report) = self.0.run(py, |job| Ok(job.report()))? else {
            return Ok(None);
        };
        let summary = PyDict::new(py);
        summary.set_item("units", report.inventory.units)?;
        summary.set_item("decoded_bytes", report.inventory.decoded_bytes)?;
        summary.set_item(
            "image_digest",
            PyBytes::new(py, &report.inventory.image_digest.as_bytes()),
        )?;
        Ok(Some(summary.unbind()))
    }

    fn advance(
        &self,
        py: Python<'_>,
        maximum_positions: usize,
        maximum_bytes: usize,
    ) -> PyResult<&'static str> {
        self.0
            .run(py, |job| job.advance(maximum_positions, maximum_bytes))
            .map(BackupCaptureStep::as_str)
    }

    fn publish(&self, py: Python<'_>) -> PyResult<Py<PyBytes>> {
        let root = self.0.run(py, OwnedBackupJob::publish)?;
        Ok(id_bytes(py, root.backup().as_bytes()))
    }

    fn finish(&self, py: Python<'_>) -> PyResult<Py<PyBytes>> {
        let root = self.0.run(py, OwnedBackupJob::finish)?;
        Ok(id_bytes(py, root.backup().as_bytes()))
    }

    fn cancel(&self, py: Python<'_>) -> PyResult<Option<Py<PyBytes>>> {
        let root = self.0.run(py, OwnedBackupJob::cancel)?;
        Ok(root.map(|root| id_bytes(py, root.backup().as_bytes())))
    }
}

#[pyclass(name = "RestoreJob", module = "uqa._uqa", frozen)]
pub(super) struct PyRestoreJob(Handle<OwnedRestoreJob>);

impl PyRestoreJob {
    pub(super) fn new(job: OwnedRestoreJob) -> Self {
        Self(Handle::new(job))
    }
}

pub(super) fn restored_engine(restored: RestoredEngine) -> PyEngine {
    PyEngine {
        inner: Some(restored.into_retained()),
    }
}

#[pymethods]
impl PyRestoreJob {
    fn close(&self, py: Python<'_>) -> PyResult<()> {
        self.0.close(py)
    }

    #[getter]
    fn id(&self, py: Python<'_>) -> PyResult<Option<Py<PyBytes>>> {
        let id = self.0.run(py, |job| Ok(job.id()))?;
        Ok(id.map(|id| id_bytes(py, id.as_bytes())))
    }

    #[getter]
    fn backup_id(&self, py: Python<'_>) -> PyResult<Py<PyBytes>> {
        let id = self.0.run(py, |job| Ok(job.backup()))?;
        Ok(id_bytes(py, id.as_bytes()))
    }

    #[getter]
    fn phase(&self, py: Python<'_>) -> PyResult<&'static str> {
        self.0.run(py, |job| Ok(job.phase().as_str()))
    }

    fn advance(&self, py: Python<'_>, maximum_object_bytes: usize) -> PyResult<&'static str> {
        self.0
            .run(py, |job| job.advance(maximum_object_bytes))
            .map(RestorePhase::as_str)
    }

    fn advance_publication(
        &self,
        py: Python<'_>,
        maximum_steps: usize,
    ) -> PyResult<Option<PyEngine>> {
        self.0
            .run(py, |job| job.advance_publication(maximum_steps))
            .map(|result| result.map(restored_engine))
    }

    fn publish(&self, py: Python<'_>) -> PyResult<PyEngine> {
        self.0
            .run(py, OwnedRestoreJob::publish)
            .map(restored_engine)
    }

    fn finish(&self, py: Python<'_>) -> PyResult<PyEngine> {
        self.0.run(py, OwnedRestoreJob::finish).map(restored_engine)
    }

    fn cancel(&self, py: Python<'_>) -> PyResult<Option<PyEngine>> {
        self.0
            .run(py, OwnedRestoreJob::cancel)
            .map(|result| result.map(restored_engine))
    }
}
