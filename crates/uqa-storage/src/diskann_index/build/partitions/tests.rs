//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::tests::{generation, version};
use super::super::DiskANNTemporaryBudget;
use super::*;
use crate::diskann_index::NavigationInput;
use std::collections::BTreeSet;

mod resources;

fn options(capacity: usize, depth: u8) -> DiskANNPartitionOptions {
    DiskANNPartitionOptions {
        max_partition_points: capacity,
        coarse_training: PQTrainingOptions {
            max_samples: 64,
            max_iterations: 3,
            max_centroids: 4,
            seed: 42,
        },
        max_depth: depth,
    }
}

fn parameters(dimensions: u32) -> DiskANNIndexParams {
    let mut parameters = DiskANNIndexParams::for_dimensions(dimensions).unwrap();
    parameters.max_degree = 4;
    parameters.build_list_size = 8;
    parameters
}

fn capture(
    directory: &Path,
    temporary: &DiskANNTemporaryBudget,
    control: &StorageReadControl,
    count: u64,
    duplicate: bool,
) -> DiskANNBuildInput {
    DiskANNBuildInput::capture(
        super::super::tests::generation(),
        32,
        directory,
        temporary,
        control,
        |visitor| {
            for id in 0..count {
                let mut raw = [0.0; 32];
                let axis = if duplicate { 0 } else { id as usize % 4 };
                raw[axis % 2] = if axis < 2 { 1.0 } else { -1.0 };
                visitor(id + 10, 0, version(), &raw)?;
            }
            Ok(())
        },
    )
    .unwrap()
}

fn edges(runs: &DiskANNPartitionRuns) -> Vec<(u64, u64)> {
    let mut output = Vec::new();
    runs.visit_edges(&mut |source, neighbor| {
        output.push((source, neighbor));
        Ok(())
    })
    .unwrap();
    assert_eq!(output.len() as u64, runs.summary.edges);
    output
}

#[test]
fn two_point_partitions_translate_both_directions_to_original_global_ids() {
    let directory = tempfile::tempdir().unwrap();
    let control = StorageReadControl::with_limit(64 << 10);
    let temporary = DiskANNTemporaryBudget::new(1 << 20);
    let input = capture(directory.path(), &temporary, &control, 5, true);
    let runs = input
        .build_partitions(directory.path(), parameters(32), options(2, 0))
        .unwrap();
    assert_eq!(runs.summary.partitions, 4);
    assert_eq!(runs.summary.memberships, 8);
    assert_eq!(
        edges(&runs),
        [
            (0, 1),
            (1, 0),
            (1, 2),
            (2, 1),
            (2, 3),
            (3, 2),
            (3, 4),
            (4, 3)
        ]
    );
    drop((input, runs));
    assert_eq!(temporary.used(), 0);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn coarse_ties_capacity_overlap_and_child_seeds_match_independent_expectations() {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../tests/fixtures/diskann/partitions.json"
    ))
    .unwrap();
    let control = StorageReadControl::with_limit(1 << 20);
    let centers = [-1.0, 0.0, 0.0, 1.0, 1.0, 0.0, 0.0, -1.0];
    for (index, raw) in [[1.0, 0.0], [3.0, 4.0], [-1.0, 0.0], [0.0, -1.0]]
        .into_iter()
        .enumerate()
    {
        let NavigationInput::Navigable(vector) =
            NavigationInput::from_raw(2, &raw, &control).unwrap()
        else {
            unreachable!()
        };
        let expected: Vec<_> = fixture["nearest_two"][index]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_u64().unwrap() as u8)
            .collect();
        assert_eq!(
            source::nearest_two(vector.coordinates(), &centers, &control)
                .unwrap()
                .as_slice(),
            expected
        );
    }
    assert_eq!(
        source::nearest_two(&[1.0, 0.0], &[1.0, 0.0, 1.0, 0.0, 1.0, 0.0], &control).unwrap(),
        [0, 1]
    );
    for (count, capacity, name) in [(10, 4, "capacity_four"), (5, 2, "capacity_two")] {
        let mut actual = Vec::new();
        leaf::windows(Ids::All(count), capacity, &control, &mut |ids| {
            actual.push(ids.to_vec());
            Ok(())
        })
        .unwrap();
        assert_eq!(serde_json::to_value(actual).unwrap(), fixture[name]);
    }
    for value in fixture["child_seeds"].as_array().unwrap() {
        assert_eq!(
            source::child_seed(42, value[0].as_u64().unwrap() as u8),
            value[1].as_u64().unwrap()
        );
    }
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn partition_graphs_exceed_build_memory_without_a_global_identity_directory() {
    let directory = tempfile::tempdir().unwrap();
    let control = StorageReadControl::with_limit(64 << 10);
    let temporary = DiskANNTemporaryBudget::new(4 << 20);
    let input = capture(directory.path(), &temporary, &control, 1024, false);
    let retained_files = temporary.used();
    let runs = input
        .build_partitions(directory.path(), parameters(32), options(32, 4))
        .unwrap();
    assert_eq!(runs.summary.coverage, input.coverage());
    assert!(runs.summary.partitions > 1);
    assert!(runs.summary.maximum_depth > 0);
    assert!(runs.summary.maximum_depth <= 4);
    assert!(runs.summary.maximum_partition_points <= 32);
    assert!(runs.summary.memberships >= 1024);
    assert!(1024 * 32 * 4 > control.memory().limit());
    assert!(control.memory().peak() <= 64 << 10);
    let candidates = edges(&runs);
    let sources: BTreeSet<_> = candidates.iter().map(|&(source, _)| source).collect();
    assert_eq!(sources, (0..1024).collect());
    assert!(candidates
        .iter()
        .all(|&(source, neighbor)| source < 1024 && neighbor < 1024 && source != neighbor));
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 3);
    assert_eq!(
        temporary.used(),
        retained_files + std::fs::metadata(runs.edges.path()).unwrap().len()
    );
    drop(input);
    assert_eq!(edges(&runs), candidates);
    drop(runs);
    assert_eq!(temporary.used(), 0);
    assert_eq!(control.memory().used(), 0);
    assert!(std::fs::read_dir(directory.path())
        .unwrap()
        .next()
        .is_none());
}

#[test]
fn duplicate_populations_and_depth_limits_keep_bounded_deterministic_windows() {
    let directory = tempfile::tempdir().unwrap();
    let control = StorageReadControl::with_limit(64 << 10);
    let temporary = DiskANNTemporaryBudget::new(1 << 20);
    let input = capture(directory.path(), &temporary, &control, 10, true);
    for depth in [0, 4] {
        let first = input
            .build_partitions(directory.path(), parameters(32), options(4, depth))
            .unwrap();
        let summary = *first.summary();
        let first_edges = edges(&first);
        assert_eq!(summary.partitions, 3);
        assert_eq!(summary.memberships, 12);
        assert_eq!(summary.maximum_partition_points, 4);
        assert_eq!(summary.maximum_depth, 0);
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 3);
        drop(first);
        let second = input
            .build_partitions(directory.path(), parameters(32), options(4, depth))
            .unwrap();
        assert_eq!(summary, *second.summary());
        assert_eq!(first_edges, edges(&second));
    }
    drop(input);
    assert_eq!(temporary.used(), 0);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn empty_numeric_side_and_singleton_inputs_keep_their_canonical_coverage() {
    for raw in [None, Some([0.0, 0.0]), Some([1.0, 0.0])] {
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
                if let Some(raw) = raw {
                    visitor(10, 0, version(), &raw)?;
                }
                Ok(())
            },
        )
        .unwrap();
        let runs = input
            .build_partitions(directory.path(), parameters(2), options(4, 4))
            .unwrap();
        assert_eq!(runs.summary.coverage, input.coverage());
        assert_eq!(runs.summary.partitions, input.node_count());
        assert_eq!(runs.summary.memberships, input.node_count());
        assert!(edges(&runs).is_empty());
        drop((runs, input));
        assert_eq!(temporary.used(), 0);
        assert_eq!(control.memory().used(), 0);
    }
}
