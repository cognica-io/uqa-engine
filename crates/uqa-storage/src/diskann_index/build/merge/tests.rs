//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::tests::{generation, version};
use super::super::{DiskANNPartitionOptions, DiskANNTemporaryBudget};
use super::*;
use crate::diskann_index::PQTrainingOptions;
use crate::vector_index::{DiskANNAlpha, DiskANNIndexParams};

mod resources;

fn parameters(dimensions: u32) -> DiskANNIndexParams {
    let mut parameters = DiskANNIndexParams::for_dimensions(dimensions).unwrap();
    parameters.max_degree = 3;
    parameters.alpha = DiskANNAlpha::new(1.2).unwrap();
    parameters
}

fn partition_options() -> DiskANNPartitionOptions {
    DiskANNPartitionOptions {
        max_partition_points: 2,
        coarse_training: PQTrainingOptions {
            max_samples: 4,
            max_iterations: 1,
            max_centroids: 3,
            seed: 42,
        },
        max_depth: 0,
    }
}

fn capture(
    directory: &Path,
    temporary: &DiskANNTemporaryBudget,
    control: &StorageReadControl,
    count: u64,
) -> DiskANNBuildInput {
    DiskANNBuildInput::capture(generation(), 32, directory, temporary, control, |visitor| {
        for id in 0..count {
            let mut raw = [0.0; 32];
            raw[id as usize % 4] = 1.0;
            visitor(id + 10, 0, version(), &raw)?;
        }
        Ok(())
    })
    .unwrap()
}

fn collect(graph: &DiskANNMergedGraph) -> Vec<Vec<u64>> {
    let mut rows = Vec::new();
    graph
        .visit_neighbors(&mut |node, neighbors| {
            assert_eq!(node, rows.len() as u64);
            rows.push(neighbors.to_vec());
            Ok(())
        })
        .unwrap();
    assert_eq!(
        rows.iter().map(|r| r.len() as u64).sum::<u64>(),
        graph.summary.edges
    );
    rows
}

#[test]
fn global_pruning_matches_independent_rational_fixture_across_sort_capacities() {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../tests/fixtures/diskann/merge.json"
    ))
    .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let control = StorageReadControl::with_limit(64 << 10);
    let temporary = DiskANNTemporaryBudget::new(1 << 20);
    let input = DiskANNBuildInput::capture(
        generation(),
        2,
        directory.path(),
        &temporary,
        &control,
        |visitor| {
            for (doc, ordinal, raw) in [
                (11, 0, [1.0, 0.0]),
                (11, 1, [-1.0, 0.0]),
                (20, 0, [3.0, 4.0]),
                (30, 0, [0.0, -1.0]),
                (99, 0, [0.0, 1.0]),
                (100, 0, [0.0, -1.0]),
            ] {
                visitor(doc, ordinal, version(), &raw)?;
            }
            Ok(())
        },
    )
    .unwrap();
    let candidates = input
        .build_partitions(directory.path(), parameters(2), partition_options())
        .unwrap();
    let (discard, mut summary) = candidates.into_source(&input).unwrap();
    drop(discard);
    summary.edges = fixture["candidate_edges"].as_array().unwrap().len() as u64;
    let mut digest = None;
    for (capacity, passes) in [(1, 4), (2, 3), (3, 3), (100, 0)] {
        let mut writer = RunWriter::new(directory.path(), &temporary, &control).unwrap();
        for edge in fixture["candidate_edges"].as_array().unwrap() {
            writer
                .append(&edge[0].as_u64().unwrap().to_le_bytes())
                .unwrap();
            writer
                .append(&edge[1].as_u64().unwrap().to_le_bytes())
                .unwrap();
        }
        let graph = merge(
            &input,
            writer.finish().unwrap(),
            &summary,
            directory.path(),
            DiskANNMergeOptions {
                sort_buffer_records: capacity,
            },
        )
        .unwrap();
        assert_eq!(
            serde_json::to_value(collect(&graph)).unwrap(),
            fixture["neighbors"]
        );
        assert_eq!(graph.summary.merge_passes, passes);
        if let Some(expected) = digest {
            assert_eq!(graph.summary.adjacency_digest, expected);
        }
        digest = Some(graph.summary.adjacency_digest);
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 3);
        drop(graph);
        assert_eq!(control.memory().used(), 0);
    }
    drop(input);
    assert_eq!(temporary.used(), 0);
}

#[test]
fn merged_graph_exceeds_workspace_with_complete_bounded_connected_neighborhoods() {
    let directory = tempfile::tempdir().unwrap();
    let control = StorageReadControl::with_limit(64 << 10);
    let temporary = DiskANNTemporaryBudget::new(4 << 20);
    let input = capture(directory.path(), &temporary, &control, 1024);
    let partitions = input
        .build_partitions(directory.path(), parameters(32), partition_options())
        .unwrap();
    let graph = input
        .merge_partitions(
            partitions,
            directory.path(),
            DiskANNMergeOptions {
                sort_buffer_records: 31,
            },
        )
        .unwrap();
    assert!(1024 * 32 * 4 > control.memory().limit());
    assert!(control.memory().peak() <= control.memory().limit());
    assert_eq!(graph.summary.partitions.coverage, input.coverage());
    assert!(graph.summary.merge_passes > 1);
    let rows = collect(&graph);
    assert_eq!(rows.len(), 1024);
    for (id, neighbors) in rows.iter().enumerate() {
        assert!(!neighbors.is_empty() && neighbors.len() <= 3);
        assert!(neighbors.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(neighbors.iter().all(|&n| n < 1024 && n != id as u64));
        assert!(neighbors.contains(&((id as u64 + 1) % 1024)));
    }
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 3);
    drop(input);
    assert_eq!(collect(&graph), rows);
    drop(graph);
    assert_eq!(control.memory().used(), 0);
    assert_eq!(temporary.used(), 0);
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
}

#[test]
fn empty_side_and_singleton_captures_preserve_source_coverage_without_edges() {
    for raw in [None, Some([0.0, 0.0]), Some([1.0, 0.0])] {
        let directory = tempfile::tempdir().unwrap();
        let control = StorageReadControl::with_limit(32 << 10);
        let temporary = DiskANNTemporaryBudget::new(1 << 20);
        let input = DiskANNBuildInput::capture(
            generation(),
            2,
            directory.path(),
            &temporary,
            &control,
            |visitor| {
                if let Some(raw) = raw {
                    visitor(10, 0, version(), &raw)?;
                }
                Ok(())
            },
        )
        .unwrap();
        let partitions = input
            .build_partitions(directory.path(), parameters(2), partition_options())
            .unwrap();
        let graph = input
            .merge_partitions(
                partitions,
                directory.path(),
                DiskANNMergeOptions {
                    sort_buffer_records: 1,
                },
            )
            .unwrap();
        assert_eq!(graph.summary.partitions.coverage, input.coverage());
        assert_eq!(collect(&graph).len() as u64, input.node_count());
        assert_eq!(graph.summary.edges, 0);
        drop((graph, input));
        assert_eq!(temporary.used(), 0);
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn small_merge_ranges_reuse_cipher_blocks_across_the_complete_pass() {
    let directory = tempfile::tempdir().unwrap();
    let control = StorageReadControl::with_limit(64 << 10);
    let temporary = DiskANNTemporaryBudget::new(1 << 20);
    let input = capture(directory.path(), &temporary, &control, 701);
    let mut writer = RunWriter::new(directory.path(), &temporary, &control).unwrap();
    for id in 0_u64..700 {
        writer.append(&0_u64.to_le_bytes()).unwrap();
        writer.append(&(id + 1).to_le_bytes()).unwrap();
        writer.append(&(id as f64).to_bits().to_le_bytes()).unwrap();
    }
    let run = writer.finish().unwrap();
    assert_eq!(run.block_io_counts().0, 0);
    let merged = sort::merge_pass(&input, &run, 700, 1, directory.path()).unwrap();
    assert_eq!(run.block_io_counts().0, 10); // Two readers, five blocks each, across 350 pairs.
    merged
        .read(&control, |file| {
            let mut range = sort::Range::new(file, 0, 700, input.node_count(), &control)?;
            for id in 0..700 {
                let candidate = range.next()?.unwrap();
                assert_eq!(candidate.source, 0);
                assert_eq!(candidate.neighbor, id + 1);
                assert_eq!(candidate.distance, id as f64);
            }
            assert!(range.next()?.is_none());
            Ok(())
        })
        .unwrap();
    drop((merged, run, input));
    assert_eq!(temporary.used(), 0);
    assert_eq!(control.memory().used(), 0);
}
