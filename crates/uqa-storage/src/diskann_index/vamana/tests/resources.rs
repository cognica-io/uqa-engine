//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::memory::MemoryError;

#[test]
fn failed_vamana_admission_releases_its_graph_and_all_work_buffers() {
    let input = StorageReadControl::with_limit(1 << 20);
    let vectors = sample_vectors(&input);
    let points = points(&vectors);
    let parameters = DiskANNIndexParams::for_dimensions(2).unwrap();
    let mut failures = 0;
    let mut successes = 0;
    for limit in [0, 1, 63, 127, 255, 511, 1023, 2047, 4095, 8191, 16383] {
        let control = StorageReadControl::with_limit(limit);
        match VamanaGraph::build(2, &points, parameters, &control) {
            Ok(graph) => {
                assert_graph(&graph, parameters.max_degree);
                successes += 1;
            }
            Err(StorageBackendError::Memory(MemoryError::Limit { .. })) => failures += 1,
            Err(error) => panic!("unexpected admission error: {error}"),
        }
        assert_eq!(control.memory().used(), 0, "limit={limit}");
    }
    assert!(failures > 0 && successes > 0);
}

#[test]
fn vamana_rejects_unordered_keys_dimensions_parameters_and_cancelled_work() {
    let input = StorageReadControl::with_limit(1 << 20);
    let vectors = sample_vectors(&input);
    let mut points = points(&vectors);
    let control = StorageReadControl::with_limit(1 << 20);
    let parameters = DiskANNIndexParams::for_dimensions(2).unwrap();
    points.swap(0, 1);
    assert!(VamanaGraph::build(2, &points, parameters, &control).is_err());
    points.swap(0, 1);
    points[1].doc_id = points[0].doc_id;
    assert!(VamanaGraph::build(2, &points, parameters, &control).is_err());
    points[1].ordinal = 1;
    assert!(VamanaGraph::build(3, &points, parameters, &control).is_err());
    let mut invalid = parameters;
    invalid.max_degree = 1;
    assert!(VamanaGraph::build(2, &points, invalid, &control).is_err());
    assert_eq!(control.memory().used(), 0);
    control.cancellation().cancel();
    assert!(matches!(
        VamanaGraph::build(2, &points, parameters, &control),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn malformed_adjacency_cannot_partially_replace_a_valid_neighborhood() {
    let control = StorageReadControl::with_limit(1 << 20);
    let vectors = sample_vectors(&control);
    let points = points(&vectors);
    let mut graph = VamanaGraph::empty(&points, 2, &control).unwrap();
    graph.replace(0, &[1, 2], &control).unwrap();
    for neighbors in [&[1, 1][..], &[0, 2], &[1, 5], &[1, 2, 3]] {
        assert!(graph.replace(0, neighbors, &control).is_err());
        assert_eq!(graph.neighbors(0).unwrap(), [1, 2]);
    }
    assert!(graph.neighbors(u64::MAX).is_err());
    assert!(graph.logical_key(u64::MAX).is_err());
    control.cancellation().cancel();
    assert!(matches!(
        graph.replace(0, &[2, 3], &control),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert_eq!(graph.neighbors(0).unwrap(), [1, 2]);
}
