//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The same explicit construction and read allowances apply to every index owner.

use super::{
    build::{
        DiskANNBuildCapture, DiskANNBuildSink, DiskANNGenerationOptions, DiskANNMergeOptions,
        DiskANNPartitionOptions,
    },
    format::DiskANNManifest,
    pages::DiskANNReadLimits,
    DiskANNCanonicalRead,
};
use crate::{vector_index::DiskANNIndexParams, StorageBackendResult};

/// Host construction and read settings. Simultaneous old/new generations also share the owner's memory allowance; temporary input uses its separately supplied shared allowance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiskANNIndexOptions {
    pub parameters: DiskANNIndexParams,
    pub read: DiskANNReadLimits,
    pub partitions: DiskANNPartitionOptions,
    pub merge: DiskANNMergeOptions,
    pub generation: DiskANNGenerationOptions,
}

impl DiskANNIndexOptions {
    pub(crate) fn write_generation<S: DiskANNCanonicalRead>(
        self,
        capture: &DiskANNBuildCapture<S>,
        directory: &std::path::Path,
        sink: &mut dyn DiskANNBuildSink,
    ) -> StorageBackendResult<DiskANNManifest> {
        let runs = capture
            .input()
            .build_partitions(directory, self.parameters, self.partitions)?;
        let graph = capture
            .input()
            .merge_partitions(runs, directory, self.merge)?;
        capture.write_generation(&graph, self.generation, sink)
    }
}
