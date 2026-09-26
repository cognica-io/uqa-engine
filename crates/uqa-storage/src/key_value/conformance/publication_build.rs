//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::diskann_index::{
    build::{
        DiskANNBuildCapture, DiskANNCanonicalCoverage, DiskANNGenerationOptions,
        DiskANNMergeOptions, DiskANNPartitionOptions, DiskANNTemporaryBudget,
    },
    DiskANNCanonicalRead, PQTrainingOptions,
};
use crate::key_value::KeyValueDiskANNStage;
use crate::{
    read_control::StorageReadControl, vector_index::DiskANNIndexParams, StorageBackendResult,
};

/// Construct and actually seal a small disposable publication fixture using the real bounded builder. No catalog head is selected; the returned evidence retains the supplied source.
pub fn build_diskann_publication_fixture<S: DiskANNCanonicalRead>(
    source: S,
    stage: &mut KeyValueDiskANNStage,
    parameters: DiskANNIndexParams,
    control: &StorageReadControl,
) -> StorageBackendResult<DiskANNCanonicalCoverage<S>> {
    let directory = tempfile::tempdir()
        .map_err(|error| crate::StorageBackendError::Other(error.to_string()))?;
    let temporary = DiskANNTemporaryBudget::new(1 << 20);
    stage.start(control)?;
    let capture = DiskANNBuildCapture::capture(
        stage.generation(),
        source,
        directory.path(),
        &temporary,
        control,
    )?;
    let training = PQTrainingOptions {
        max_samples: 8,
        max_centroids: 2,
        max_iterations: 2,
        seed: 42,
    };
    let partitions = capture.input().build_partitions(
        directory.path(),
        parameters,
        DiskANNPartitionOptions {
            max_partition_points: 8,
            coarse_training: PQTrainingOptions {
                max_centroids: 3,
                ..training
            },
            max_depth: 0,
        },
    )?;
    let graph = capture.input().merge_partitions(
        partitions,
        directory.path(),
        DiskANNMergeOptions {
            sort_buffer_records: 8,
        },
    )?;
    let manifest = capture.write_generation(
        &graph,
        DiskANNGenerationOptions {
            training,
            code_batch_nodes: 4,
            side_batch_entries: 4,
            max_record_bytes: 8192,
        },
        stage,
    )?;
    drop(stage.seal(manifest, 8192, control)?);
    capture.finish(&manifest, control)
}
