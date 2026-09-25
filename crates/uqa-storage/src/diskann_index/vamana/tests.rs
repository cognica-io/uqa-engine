//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::diskann_index::NavigationInput;

mod resources;

fn vectors(raw: &[[f32; 2]], control: &StorageReadControl) -> Vec<NavigationVector> {
    raw.iter()
        .map(
            |raw| match NavigationInput::from_raw(2, raw, control).unwrap() {
                NavigationInput::Navigable(vector) => vector,
                NavigationInput::Exact(reason) => panic!("unexpected side entry: {reason:?}"),
            },
        )
        .collect()
}

fn points(vectors: &[NavigationVector]) -> Vec<VamanaPoint<'_>> {
    vectors
        .iter()
        .enumerate()
        .map(|(index, vector)| VamanaPoint {
            doc_id: index as u64 + 1,
            ordinal: 0,
            vector,
        })
        .collect()
}

fn adjacency(graph: &VamanaGraph) -> Vec<Vec<u64>> {
    (0..graph.len())
        .map(|node| {
            let mut neighbors = graph.neighbors(node as u64).unwrap().to_vec();
            neighbors.sort_unstable();
            neighbors
        })
        .collect()
}

fn reference() -> serde_json::Value {
    serde_json::from_str(include_str!(
        "../../../tests/fixtures/diskann/reference.json"
    ))
    .unwrap()
}

fn integer(value: &serde_json::Value) -> u64 {
    value.as_u64().unwrap()
}
fn ids(value: &serde_json::Value) -> Vec<u64> {
    value.as_array().unwrap().iter().map(integer).collect()
}
fn rows(value: &serde_json::Value) -> Vec<Vec<u64>> {
    value.as_array().unwrap().iter().map(ids).collect()
}

fn sample_vectors(control: &StorageReadControl) -> Vec<NavigationVector> {
    vectors(
        &[[1.0, 0.0], [-1.0, 0.0], [3.0, 4.0], [0.0, -1.0], [0.0, 1.0]],
        control,
    )
}

#[test]
fn robust_pruning_matches_independent_geometry_and_squared_alpha_units() {
    let control = StorageReadControl::with_limit(1 << 20);
    let vectors = vectors(&[[1.0, 0.0], [3.0, 4.0], [-1.0, 0.0]], &control);
    let points = points(&vectors);
    let fixture = &reference()["pruning"];
    let alpha = DiskANNAlpha::new(6.0 / 5.0).unwrap();
    let candidates = ids(&fixture["candidates"]);
    assert_eq!(
        &*prune::select(&points, 0, &candidates, &[], alpha, 2, &control).unwrap(),
        ids(&fixture["expected_neighbors"])
    );
    assert_eq!(
        &*prune::select(
            &points,
            0,
            &candidates,
            &[],
            DiskANNAlpha::new(1.0).unwrap(),
            2,
            &control
        )
        .unwrap(),
        ids(&fixture["alpha_one_neighbors"])
    );
    assert_eq!(
        &*prune::select(&points, 0, &[2], &[1, 1, 0], alpha, 2, &control).unwrap(),
        [1, 2]
    );
    assert!(prune::select(&points, 0, &[1, 2], &[], alpha, 0, &control)
        .unwrap()
        .is_empty());
    assert!(prune::select(&points, 0, &[99], &[], alpha, 2, &control).is_err());
}

#[test]
fn both_vamana_passes_and_reserved_cycle_match_the_preexisting_rational_fixture() {
    let control = StorageReadControl::with_limit(1 << 20);
    let vectors = sample_vectors(&control);
    let mut points = points(&vectors);
    let fixture = &reference()["graph"];
    for (point, key) in points.iter_mut().zip(rows(&fixture["logical_keys"])) {
        point.doc_id = key[0];
        point.ordinal = u32::try_from(key[1]).unwrap();
    }
    let mut graph = VamanaGraph::empty(&points, 3, &control).unwrap();
    graph.entry = Some(integer(&fixture["entry"]));
    for (source, neighbors) in rows(&fixture["initial_neighbors"]).iter().enumerate() {
        graph.replace(source as u64, neighbors, &control).unwrap();
    }
    for (pass, alpha) in [1.0, 2.0].into_iter().enumerate() {
        graph
            .refine(
                &points,
                &ids(&fixture["visit_order"]),
                4,
                DiskANNAlpha::new(alpha).unwrap(),
                &control,
            )
            .unwrap();
        assert_eq!(
            adjacency(&graph),
            rows(&fixture["unaugmented_passes"][pass])
        );
    }
    graph
        .connect(&points, DiskANNAlpha::new(2.0).unwrap(), &control)
        .unwrap();
    assert_eq!(adjacency(&graph), rows(&fixture["with_reserved_cycle"]));
    for (node, point) in points.iter().enumerate() {
        assert_eq!(
            graph.logical_key(node as u64).unwrap(),
            (point.doc_id, point.ordinal)
        );
    }
}

#[test]
fn construction_keeps_all_visited_nodes_after_the_frontier_evicts_them() {
    let control = StorageReadControl::with_limit(1 << 20);
    let vectors = vectors(
        &[[1.0, 0.0], [3.0, 4.0], [0.0, 1.0], [-3.0, 4.0], [-1.0, 0.0]],
        &control,
    );
    let points = points(&vectors);
    let mut graph = VamanaGraph::empty(&points, 2, &control).unwrap();
    graph.entry = Some(4);
    for source in 1..5 {
        graph.replace(source, &[source - 1], &control).unwrap();
    }
    let mut work = search::Workspace::new(5, 1, 2, &control).unwrap();
    assert_eq!(
        work.visited(&graph, &points, 0, &control).unwrap(),
        [4, 3, 2, 1, 0]
    );
}

#[test]
fn initialization_matches_independent_seed_and_capped_centroid_expectations() {
    use sha2::{Digest, Sha256};
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/diskann/initialization.json"
    ))
    .unwrap();
    let control = StorageReadControl::with_limit(1 << 20);
    let vectors = sample_vectors(&control);
    let points = points(&vectors);
    let seed = integer(&fixture["seed"]);
    let mut graph = VamanaGraph::empty(&points, 3, &control).unwrap();
    initialize::edges(&mut graph, seed, &control).unwrap();
    assert_eq!(
        adjacency(&graph),
        rows(&fixture["small"]["initial_neighbors"])
    );
    assert_eq!(
        &*initialize::permutation(points.len(), seed, &control).unwrap(),
        ids(&fixture["small"]["visit_order"])
    );
    assert_eq!(
        initialize::entry(&points, seed, &control).unwrap(),
        integer(&fixture["small"]["entry"])
    );
    let axes = [[1.0, 0.0], [0.0, 1.0], [-1.0, 0.0], [0.0, -1.0]];
    let raw: Vec<_> = (0..260).map(|index| axes[index % 4]).collect();
    let vectors = self::vectors(&raw, &control);
    let points = self::points(&vectors);
    let sample = initialize::entry_sample(points.len(), seed, &control).unwrap();
    assert_eq!(
        sample.len() as u64,
        integer(&fixture["capped"]["sample_count"])
    );
    let mut digest = Sha256::new();
    for node in &*sample {
        digest.update(node.to_le_bytes());
    }
    assert_eq!(
        format!("{:x}", digest.finalize()),
        fixture["capped"]["sample_u64le_sha256"].as_str().unwrap()
    );
    assert_eq!(
        initialize::entry(&points, seed, &control).unwrap(),
        integer(&fixture["capped"]["entry"])
    );
}

#[test]
fn degenerate_populations_keep_degree_identity_reachability_and_deterministic_edges() {
    let input = StorageReadControl::with_limit(1 << 20);
    for count in [0, 1, 2, 3, 17] {
        for duplicate in [false, true] {
            let raw: Vec<_> = (0..count)
                .map(|index| {
                    if duplicate {
                        [1.0, 0.0]
                    } else {
                        [1.0, index as f32]
                    }
                })
                .collect();
            let vectors = vectors(&raw, &input);
            let points = points(&vectors);
            for degree in [2, 4, 64] {
                let control = StorageReadControl::with_limit(1 << 20);
                let mut parameters = DiskANNIndexParams::for_dimensions(2).unwrap();
                parameters.max_degree = degree;
                let graph = VamanaGraph::build(2, &points, parameters, &control).unwrap();
                let again = VamanaGraph::build(2, &points, parameters, &control).unwrap();
                assert_eq!(graph.entry(), again.entry());
                assert_eq!(adjacency(&graph), adjacency(&again));
                assert_graph(&graph, degree);
                drop(graph);
                drop(again);
                assert_eq!(control.memory().used(), 0);
            }
        }
    }
}

fn assert_graph(graph: &VamanaGraph, degree: usize) {
    assert_eq!(graph.entry().is_none(), graph.is_empty());
    let mut reached = vec![false; graph.len()];
    let mut pending: Vec<_> = graph.entry().into_iter().collect();
    while let Some(node) = pending.pop() {
        if reached[node as usize] {
            continue;
        }
        reached[node as usize] = true;
        let neighbors = graph.neighbors(node).unwrap();
        assert!(neighbors.len() <= degree);
        for (index, &neighbor) in neighbors.iter().enumerate() {
            assert_ne!(node, neighbor);
            assert!(neighbor < graph.len() as u64);
            assert!(!neighbors[..index].contains(&neighbor));
            pending.push(neighbor);
        }
        if graph.len() > 1 {
            assert!(neighbors.contains(&((node + 1) % graph.len() as u64)));
        }
    }
    assert!(reached.into_iter().all(|seen| seen));
}
