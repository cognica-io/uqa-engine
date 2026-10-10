//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{hnsw_index::HNSWRestoreBuilder, read_control::StorageReadControl};

fn ring(dimensions: usize, count: u64, control: &StorageReadControl) -> HNSWRestoreBuilder {
    let params = HNSWIndexParams {
        m: 2,
        ef_search: count as usize,
        ..HNSWIndexParams::default()
    };
    let meta = super::super::HNSWGraphMeta {
        entry_point: Some(1),
        max_level: 0,
        next_node_id: count + 1,
        live_count: count as usize,
        deleted_count: 0,
    };
    let mut builder = HNSWRestoreBuilder::new(dimensions as u32, params, meta, control).unwrap();
    for id in 1..=count {
        builder
            .push(super::super::HNSWNodeSnapshot {
                node_id: id,
                doc_id: id,
                vector_ordinal: 0,
                raw_vector: vector(id, dimensions),
                level: 0,
                deleted: false,
                neighbors: vec![vec![
                    if id == 1 { count } else { id - 1 },
                    if id == count { 1 } else { id + 1 },
                ]],
            })
            .unwrap();
    }
    builder
}

#[test]
fn spilled_topology_validation_does_not_decode_dense_vectors() {
    for dimensions in [256, 1024] {
        let control = StorageReadControl::with_limit(128 * 1024);
        let builder = ring(dimensions, 128, &control);
        super::super::store::DECODED_VECTOR_FLOATS.set(0);
        let restored = builder.finish().unwrap();
        assert!(restored.raw_vectors.is_spilled());
        assert!(restored.normalized_vectors.is_spilled());
        assert!(restored.nodes.is_spilled());
        assert_eq!(
            super::super::store::DECODED_VECTOR_FLOATS.get(),
            0,
            "topology validation decoded dense vector components"
        );
        restored.validate_invariants().unwrap();
        assert_eq!(super::super::store::DECODED_VECTOR_FLOATS.get(), 0);
        drop(restored);
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn topology_validation_rejects_missing_or_readdressed_vectors_without_decoding_them() {
    let control = StorageReadControl::with_limit(128 * 1024);
    for normalized in [false, true] {
        let mut restored = ring(256, 128, &control).finish().unwrap().into_parts().0;
        let vectors = if normalized {
            &mut restored.normalized_vectors
        } else {
            &mut restored.raw_vectors
        };
        vectors.remove(1, Some(&control)).unwrap();
        super::super::store::DECODED_VECTOR_FLOATS.set(0);
        assert!(restored.validate_invariants().is_err());
        assert_eq!(super::super::store::DECODED_VECTOR_FLOATS.get(), 0);
        let vectors = if normalized {
            &mut restored.normalized_vectors
        } else {
            &mut restored.raw_vectors
        };
        vectors
            .insert(
                129,
                super::super::types::HNSWVector {
                    values: vector(1, 256),
                    norm: 1.0,
                },
                Some(&control),
            )
            .unwrap();
        assert!(restored.validate_invariants().is_err());
        assert_eq!(super::super::store::DECODED_VECTOR_FLOATS.get(), 0);
    }
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn spilled_search_reads_only_the_vector_used_by_each_distance_or_score() {
    let dimensions = 1024;
    let count = 128;
    let control = StorageReadControl::with_limit(128 * 1024);
    let restored = ring(dimensions, count, &control).finish().unwrap();
    assert!(restored.raw_vectors.is_spilled());
    assert!(restored.normalized_vectors.is_spilled());
    let resident_control = StorageReadControl::with_limit(8 * 1024 * 1024);
    let resident = ring(dimensions, count, &resident_control).finish().unwrap();
    let query = vector(19, dimensions);
    let expected = resident.search_knn(&query, 7).unwrap();
    super::super::store::DECODED_VECTOR_FLOATS.set(0);
    for _ in 0..2 {
        let before = super::super::store::DECODED_VECTOR_FLOATS.get();
        assert_eq!(restored.search_knn(&query, 7).unwrap(), expected);
        let decoded = super::super::store::DECODED_VECTOR_FLOATS.get() - before;
        assert!(
            decoded <= 2 * count as usize * dimensions,
            "decoded {decoded} components for {count} candidate distances and canonical scores"
        );
    }
    assert!(control.memory().peak() <= control.memory().limit());
    drop(restored);
    assert_eq!(control.memory().used(), 0);
}

fn assert_same_graph(first: &HNSWIndex, second: &HNSWIndex, control: &StorageReadControl) {
    let mut first = first.delta(control);
    let mut second = second.delta(control);
    first.full_rewrite = true;
    second.full_rewrite = true;
    assert_eq!(first.meta, second.meta);
    let mut first_nodes = first.nodes();
    let mut second_nodes = second.nodes();
    loop {
        match (first_nodes.next(), second_nodes.next()) {
            (Some(first), Some(second)) => assert_eq!(*first.unwrap(), *second.unwrap()),
            (None, None) => break,
            _ => panic!("different graph cardinality"),
        }
    }
}

#[test]
fn unchanged_pruning_writes_no_topology_and_combined_connections_preserve_the_graph() {
    let control = StorageReadControl::with_limit(128 * 1024);
    let mut combined = ring(256, 128, &control).finish().unwrap().into_parts().0;
    assert!(combined.nodes.is_spilled());
    let retained = combined.clone();
    super::super::store::ENCODED_NODES.set(0);
    super::super::store::DECODED_VECTOR_FLOATS.set(0);
    for id in 2..128 {
        combined.prune_node(id, 0, Some(&control)).unwrap();
    }
    assert_eq!(super::super::store::ENCODED_NODES.get(), 0);
    assert_eq!(super::super::store::DECODED_VECTOR_FLOATS.get(), 0);
    assert_same_graph(&combined, &retained, &control);
    assert!(combined.take_persistence_delta().nodes().next().is_none());
    let mut reference = combined.clone();
    let mut combined_writes = 0;
    let mut reference_writes = 0;
    for neighbor in 66..74 {
        super::super::store::ENCODED_NODES.set(0);
        // The former algorithm published the tentative connection before pruning it.
        reference
            .modify_node(64, Some(&control), |node| {
                if !node.neighbors[0].contains(&neighbor) {
                    node.neighbors[0].push(neighbor);
                }
            })
            .unwrap();
        reference.prune_node(64, 0, Some(&control)).unwrap();
        reference_writes += super::super::store::ENCODED_NODES.get();
        super::super::store::ENCODED_NODES.set(0);
        combined
            .connect_and_prune_node(64, neighbor, 0, Some(&control))
            .unwrap();
        combined_writes += super::super::store::ENCODED_NODES.get();
        assert_same_graph(&combined, &reference, &control);
    }
    assert!(
        combined_writes < reference_writes,
        "{combined_writes} versus {reference_writes}"
    );
    for id in 1..=128 {
        assert_eq!(
            retained.node(id).unwrap().unwrap().neighbors,
            vec![vec![
                if id == 1 { 128 } else { id - 1 },
                if id == 128 { 1 } else { id + 1 }
            ]]
        );
        assert_eq!(retained.raw_vector(id).unwrap().values, vector(id, 256));
    }
    assert_eq!(
        combined.search_knn(&vector(64, 256), 7).unwrap(),
        reference.search_knn(&vector(64, 256), 7).unwrap()
    );
    assert!(control.memory().peak() <= control.memory().limit());
    drop((combined, reference, retained));
    assert_eq!(control.memory().used(), 0);
}
