//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{source::Canonical, DiskANNMemoryOptions};
use crate::diskann_index::{
    build::{DiskANNBuildCapture, DiskANNTemporaryBudget},
    format::DiskANNGeneration,
    pages::DiskANNMemoryBuilder,
    RetainedDiskANNIndex,
};
use crate::{read_control::StorageReadControl, StorageBackendError, StorageBackendResult};
use std::sync::Arc;

pub(super) fn prepare(
    source: Canonical,
    generation: DiskANNGeneration,
    options: DiskANNMemoryOptions,
    temporary: &DiskANNTemporaryBudget,
    control: &StorageReadControl,
) -> StorageBackendResult<RetainedDiskANNIndex<Canonical>> {
    control.check()?;
    let directory =
        tempfile::tempdir().map_err(|error| StorageBackendError::Other(error.to_string()))?;
    let capture = DiskANNBuildCapture::capture(
        generation,
        source.clone(),
        directory.path(),
        temporary,
        control,
    )?;
    let mut sink = DiskANNMemoryBuilder::new(generation, control.memory());
    let manifest = options.write_generation(&capture, directory.path(), &mut sink)?;
    let physical = sink.finish(manifest, control)?;
    drop(capture.finish(&manifest, control)?);
    // Clear only the candidate's changes after complete construction and sealing. A failure before replacement leaves the live root and every earlier reader intact.
    RetainedDiskANNIndex::open(
        source.covered(),
        Arc::new(physical),
        options.parameters,
        options.read,
        control,
    )
}
