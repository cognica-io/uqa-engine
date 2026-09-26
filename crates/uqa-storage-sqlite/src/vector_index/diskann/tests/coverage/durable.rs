//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    canonical, open, Capture, DiskANNBuildCapture, DiskANNTemporaryBudget, StorageReadControl,
};
use uqa_storage::diskann_index::{
    build::{DiskANNGenerationOptions, DiskANNMergeOptions, DiskANNPartitionOptions},
    pages::DiskANNOriginReader,
    PQTrainingOptions,
};
use uqa_storage::key_value::KeyValueDiskANNStage;
use uqa_storage::vector_index::DiskANNIndexParams;

#[test]
fn native_diskann_complete_origins_survive_private_undo_and_cold_reopen_in_all_file_modes() {
    let directory = tempfile::tempdir().unwrap();
    let temporary = tempfile::tempdir().unwrap();
    for mode in 0..4 {
        let path = directory.path().join(format!("durable-coverage-{mode}.db"));
        let (generation, expected, summary) = {
            let control = StorageReadControl::with_limit(1 << 20);
            let connection = open(&path, mode);
            let canonical = canonical(&connection, "docs", "vector", 2);
            canonical.replace(0, &[vec![3.0, 4.0]], &control).unwrap();
            let empty = canonical.replace(1, &[], &control).unwrap();
            let vector = canonical.replace(2, &[vec![1.0, 0.0]], &control).unwrap();
            connection.begin_transaction().unwrap();
            let private = canonical.replace(0, &[], &control).unwrap();
            let source = canonical.retain(&control).unwrap();
            connection.rollback_transaction().unwrap();
            canonical.replace(1, &[vec![0.0, 1.0]], &control).unwrap();
            let repository = connection.diskann_generations(&control).unwrap();
            repository.initialize(&control).unwrap();
            let mut stage = repository.allocate_stage(31, 32, &control).unwrap();
            stage.start(&control).unwrap();
            let budget = DiskANNTemporaryBudget::new(1 << 20);
            let capture = DiskANNBuildCapture::capture(
                stage.generation(),
                source,
                temporary.path(),
                &budget,
                &control,
            )
            .unwrap();
            let summary = seal(capture, &mut stage, temporary.path(), &control);
            assert_eq!(budget.used(), 0);
            (stage.generation(), [private, empty, vector], summary)
        };
        assert!(std::fs::read_dir(temporary.path())
            .unwrap()
            .next()
            .is_none());
        let control = StorageReadControl::with_limit(64 << 10);
        let connection = open(&path, mode);
        let repository = connection.diskann_generations(&control).unwrap();
        let source = repository.open_source(generation, &control).unwrap();
        let reader = DiskANNOriginReader::open(source, 8192, &control).unwrap();
        assert_eq!(reader.manifest().origins(), Some(summary));
        for (document, version) in expected.into_iter().enumerate() {
            let origin = reader.origin(document as u64, &control).unwrap().unwrap();
            assert_eq!(origin.version(), version);
            assert_eq!(origin.count(), u64::from(document == 2));
        }
        assert_eq!(reader.origin(3, &control).unwrap(), None);
        drop((reader, repository, connection));
        assert_eq!(control.memory().used(), 0);
    }
}

fn seal(
    capture: Capture,
    stage: &mut KeyValueDiskANNStage,
    directory: &std::path::Path,
    control: &StorageReadControl,
) -> uqa_storage::diskann_index::format::DiskANNOriginSummary {
    let parameters = DiskANNIndexParams {
        max_degree: 2,
        build_list_size: 4,
        search_list_size: 4,
        beam_width: 2,
        pq_bytes: 1,
        ..DiskANNIndexParams::for_dimensions(2).unwrap()
    };
    let training = PQTrainingOptions {
        max_samples: 4,
        max_centroids: 2,
        max_iterations: 2,
        seed: 42,
    };
    let runs = capture
        .input()
        .build_partitions(
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
        )
        .unwrap();
    let graph = capture
        .input()
        .merge_partitions(
            runs,
            directory,
            DiskANNMergeOptions {
                sort_buffer_records: 4,
            },
        )
        .unwrap();
    let manifest = capture
        .write_generation(
            &graph,
            DiskANNGenerationOptions {
                training,
                code_batch_nodes: 2,
                side_batch_entries: 2,
                max_record_bytes: 8192,
            },
            stage,
        )
        .unwrap();
    drop(stage.seal(manifest, 8192, control).unwrap());
    drop(graph);
    let coverage = capture.finish(&manifest, control).unwrap();
    assert_eq!(manifest.origins(), Some(coverage.origins()));
    coverage.origins()
}
