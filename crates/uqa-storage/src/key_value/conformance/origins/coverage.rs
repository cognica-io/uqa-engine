//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Build membership on actual retained provider sources, including late commits and private undo branches.

use super::{expect, expect_eq};
use crate::diskann_index::{
    build::{
        DiskANNBuildCapture, DiskANNBuildInput, DiskANNGenerationOptions, DiskANNMergeOptions,
        DiskANNPartitionOptions, DiskANNTemporaryBudget,
    },
    format::{DiskANNChangeIdentity, DiskANNGeneration, DiskANNManifest},
    pages::DiskANNMemoryBuilder,
    PQTrainingOptions,
};
use crate::key_value::KeyValueDiskANNCanonical;
use crate::read_control::StorageReadControl;
use crate::vector_index::DiskANNIndexParams;
use crate::{KeyValueStore, StorageBackendResult};
use std::{path::Path, sync::Arc};
use uqa_core::memory::MemoryBudget;

pub(super) fn verify(store: &Arc<dyn KeyValueStore>) -> StorageBackendResult<()> {
    let source_control = StorageReadControl::with_limit(1 << 20);
    let build = StorageReadControl::with_limit(64 << 10);
    let query = StorageReadControl::with_limit(8192);
    let directory = tempfile::tempdir()
        .map_err(|error| crate::StorageBackendError::Other(error.to_string()))?;
    let temporary = DiskANNTemporaryBudget::new(1 << 20);
    let canonical = KeyValueDiskANNCanonical::new(store.clone(), "diskann-coverage", "vector", 2)?;
    let original = canonical.replace(0, &[vec![3.0, 4.0]], &source_control)?;
    let empty = canonical.replace(5, &[], &source_control)?;
    let peer = store.open_session()?;
    store.begin_transaction()?;
    let late = canonical.replace(7, &[vec![1.0, 0.0]], &source_control)?;
    let peer_index = KeyValueDiskANNCanonical::new(peer.clone(), "diskann-coverage", "vector", 2)?;
    let early = peer_index.replace(9, &[vec![0.0, -0.0]], &source_control)?;
    expect(
        late.writer().allocation() < early.writer().allocation(),
        "writer allocation differs from selected commit order",
    )?;
    let captured = DiskANNBuildCapture::capture(
        DiskANNGeneration::new([37; 16], 1, 2, 3)?,
        peer_index.retain(&source_control)?,
        directory.path(),
        &temporary,
        &build,
    )?;
    store.commit_transaction()?;
    let manifest = complete(captured.input(), directory.path(), &build)?;
    let coverage = captured.finish(&manifest, &query)?;
    drop((peer_index, peer));
    for (document, version, expected) in [
        (0, original, true),
        (5, empty, true),
        (7, late, false),
        (9, early, true),
        (99, original, false),
    ] {
        expect_eq(
            &coverage.contains(DiskANNChangeIdentity::new(document, version), &query)?,
            &expected,
            "actual selected origin membership",
        )?;
    }
    let replacement = canonical.replace(0, &[vec![0.0, 1.0]], &source_control)?;
    expect(
        !coverage.contains(DiskANNChangeIdentity::new(0, replacement), &query)?,
        "later replacement is not covered",
    )?;
    expect(
        coverage.contains(DiskANNChangeIdentity::new(0, original), &query)?,
        "selected version survives source mutation",
    )?;
    store.begin_transaction()?;
    store.savepoint("coverage")?;
    let undone = canonical.replace(0, &[], &source_control)?;
    let private = DiskANNBuildCapture::capture(
        DiskANNGeneration::new([37; 16], 1, 2, 4)?,
        canonical.retain(&source_control)?,
        directory.path(),
        &temporary,
        &build,
    )?;
    store.rollback_to_savepoint("coverage")?;
    store.rollback_transaction()?;
    let private_manifest = complete(private.input(), directory.path(), &build)?;
    let private_coverage = private.finish(&private_manifest, &query)?;
    expect(
        private_coverage.contains(DiskANNChangeIdentity::new(0, undone), &query)?,
        "retained private empty replacement remains covered",
    )?;
    expect(
        !private_coverage.contains(DiskANNChangeIdentity::new(0, replacement), &query)?,
        "undo does not retarget build coverage",
    )?;
    expect_eq(
        &temporary.used(),
        &0,
        "completed coverage releases encrypted build input",
    )?;
    expect_eq(
        &query.memory().used(),
        &0,
        "coverage membership releases query workspace",
    )?;
    source_control.cancellation().cancel();
    expect(
        coverage
            .contains(DiskANNChangeIdentity::new(99, original), &query)
            .is_err(),
        "coverage preserves original control even for absent data",
    )
}

fn complete(
    input: &DiskANNBuildInput,
    directory: &Path,
    control: &StorageReadControl,
) -> StorageBackendResult<DiskANNManifest> {
    let parameters = DiskANNIndexParams {
        max_degree: 2,
        build_list_size: 4,
        search_list_size: 4,
        beam_width: 1,
        pq_bytes: 1,
        ..DiskANNIndexParams::for_dimensions(input.dimensions())?
    };
    let training = PQTrainingOptions {
        max_samples: 4,
        max_centroids: 2,
        max_iterations: 2,
        seed: 42,
    };
    let partitions = input.build_partitions(
        directory,
        parameters,
        DiskANNPartitionOptions {
            max_partition_points: 4,
            coarse_training: PQTrainingOptions {
                max_centroids: 3,
                ..training
            },
            max_depth: 0,
        },
    )?;
    let graph = input.merge_partitions(
        partitions,
        directory,
        DiskANNMergeOptions {
            sort_buffer_records: 4,
        },
    )?;
    let physical = MemoryBudget::new(64 << 10);
    let mut sink = DiskANNMemoryBuilder::new(input.coverage().generation(), &physical);
    let manifest = input.write_generation(
        &graph,
        DiskANNGenerationOptions {
            training,
            code_batch_nodes: 2,
            side_batch_entries: 2,
            max_record_bytes: 32 << 10,
        },
        &mut sink,
    )?;
    drop(sink.finish(manifest, control)?);
    expect_eq(
        &physical.used(),
        &0,
        "physical seal releases the disposable generation",
    )?;
    Ok(manifest)
}
