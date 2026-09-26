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
    /// Default session temporary-file ceiling; cloned build owners share one live allowance.
    pub const DEFAULT_TEMPORARY_BYTES: u64 = 8 << 30;

    /// Bounded host defaults. These ceilings never enlarge the caller's original memory allowance, and admission never silently reduces the algorithm configuration.
    pub fn for_parameters(parameters: crate::vector_index::DiskANNIndexParams) -> Self {
        let training = super::PQTrainingOptions {
            seed: parameters.seed,
            ..Default::default()
        };
        Self {
            parameters,
            read: DiskANNReadLimits {
                resident_bytes: 256 << 20,
                cache_bytes: 16 << 20,
                max_in_flight_page_bytes: 1 << 20,
                max_record_bytes: 8 << 20,
            },
            partitions: DiskANNPartitionOptions {
                max_partition_points: 8192,
                coarse_training: super::PQTrainingOptions {
                    max_centroids: 8,
                    ..training
                },
                max_depth: 32,
            },
            merge: DiskANNMergeOptions {
                sort_buffer_records: 65_536,
            },
            generation: DiskANNGenerationOptions {
                training,
                code_batch_nodes: 4096,
                side_batch_entries: 4096,
                max_record_bytes: 8 << 20,
            },
        }
    }

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
