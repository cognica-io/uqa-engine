//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Complete construction retains its original source and the dedicated physical writer.

use super::KeyValueDiskANNStage;
use crate::{
    diskann_index::{
        build::{DiskANNBuildCapture, DiskANNCanonicalCoverage, DiskANNTemporaryBudget},
        DiskANNCanonicalRead, DiskANNIndexOptions,
    },
    read_control::StorageReadControl,
    StorageBackendError, StorageBackendResult,
};

impl KeyValueDiskANNStage {
    /// Capture, construct and seal a generation without publishing it or completing the caller's transaction. The caller keeps this staging owner on failure, including its original unresolved physical write attempt.
    pub fn build<S: DiskANNCanonicalRead>(
        &mut self,
        source: S,
        options: DiskANNIndexOptions,
        temporary: &DiskANNTemporaryBudget,
        control: &StorageReadControl,
    ) -> StorageBackendResult<DiskANNCanonicalCoverage<S>> {
        source.check_control(control)?;
        options.parameters.validate(source.dimensions())?;
        let directory =
            tempfile::tempdir().map_err(|error| StorageBackendError::Other(error.to_string()))?;
        self.start(control)?;
        let capture = DiskANNBuildCapture::capture(
            self.generation(),
            source,
            directory.path(),
            temporary,
            control,
        )?;
        let manifest = options.write_generation(&capture, directory.path(), self)?;
        drop(self.seal(manifest, options.generation.max_record_bytes, control)?);
        capture.finish(&manifest, control)
    }
}
