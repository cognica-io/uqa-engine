//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bounded restoration retains exact topology and rejects incomplete tensors.

use super::*;
use crate::read_control::StorageReadControl;

fn fixture() -> HNSWIndex {
    let mut index = HNSWIndex::with_params(
        4,
        HNSWIndexParams {
            rebuild_threshold: 100,
            ..HNSWIndexParams::default()
        },
    )
    .unwrap();
    for document in 1..=16 {
        index
            .add_many(document, vec![vector(document, 4), vector(document + 1, 4)])
            .unwrap();
    }
    index.delete(1).unwrap();
    index
}

#[test]
fn restoration_retains_exact_nodes_and_fails_before_graph_allocation() {
    let original = fixture().persistence_snapshot();
    let control = StorageReadControl::with_limit(1 << 20);
    let restored = HNSWIndex::from_persistence_controlled(
        4,
        fixture().params(),
        original.meta,
        original.nodes.clone(),
        &control,
    )
    .unwrap();
    assert_eq!(restored.persistence_snapshot(), original);
    assert!(control.memory().used() > 0);
    drop(restored);
    assert_eq!(control.memory().used(), 0);
    let source = fixture();
    let snapshot = source.persistence_snapshot();
    let limited = StorageReadControl::with_limit(0);
    let allocation = allocation_counter::measure(|| {
        assert!(HNSWIndex::from_persistence_controlled(
            4,
            source.params(),
            snapshot.meta,
            snapshot.nodes,
            &limited
        )
        .is_err());
    });
    assert_eq!(allocation.count_total, 0);
    assert_eq!(limited.memory().used(), 0);
    control.cancellation().cancel();
    assert!(HNSWIndex::from_persistence_controlled(
        4,
        source.params(),
        original.meta,
        original.nodes,
        &control
    )
    .is_err());
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn reconstruction_rejects_a_missing_live_tensor_ordinal() {
    let source = fixture();
    for missing_start in [false, true] {
        let mut snapshot = source.persistence_snapshot();
        for node in snapshot.nodes.iter_mut().filter(|node| node.doc_id == 2) {
            if missing_start || node.vector_ordinal == 1 {
                node.vector_ordinal += 1;
            }
        }
        let control = StorageReadControl::with_limit(1 << 20);
        let error = HNSWIndex::from_persistence_controlled(
            4,
            source.params(),
            snapshot.meta,
            snapshot.nodes,
            &control,
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("ordinals are not contiguous"),
            "{error}"
        );
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn dense_and_sparse_restored_identities_preserve_search_scores_and_tombstones() {
    let source = fixture();
    for (offset, stride) in [(u64::MAX / 2, 1), (0, 1_000_000)] {
        let remap = |id| offset + id * stride;
        let mut snapshot = source.persistence_snapshot();
        snapshot.meta.entry_point = snapshot.meta.entry_point.map(remap);
        snapshot.meta.next_node_id = remap(snapshot.meta.next_node_id);
        for node in &mut snapshot.nodes {
            node.node_id = remap(node.node_id);
            for layer in &mut node.neighbors {
                for neighbor in layer {
                    *neighbor = remap(*neighbor);
                }
            }
        }
        let restored =
            HNSWIndex::from_persistence(4, source.params(), snapshot.meta, snapshot.nodes).unwrap();
        restored.validate_invariants().unwrap();
        for seed in 0..32 {
            for k in [1, 7, 20] {
                let query = vector(seed, 4);
                let expected = source.search_knn(&query, k).unwrap();
                let actual = restored.search_knn(&query, k).unwrap();
                assert_eq!(actual, expected);
            }
        }
    }
}

#[test]
fn restoration_charges_transferred_vector_and_adjacency_capacity() {
    let source = fixture();
    let mut snapshot = source.persistence_snapshot();
    let control = StorageReadControl::with_limit(1 << 20);
    let compact = HNSWIndex::from_persistence_controlled(
        4,
        source.params(),
        snapshot.meta,
        snapshot.nodes.clone(),
        &control,
    )
    .unwrap();
    let compact_bytes = control.memory().used();
    drop(compact);
    let node = &mut snapshot.nodes[0];
    node.raw_vector.reserve_exact(4096);
    node.neighbors.reserve_exact(64);
    for layer in &mut node.neighbors {
        layer.reserve_exact(128);
    }
    let raw_capacity = node.raw_vector.capacity();
    let node_id = node.node_id;
    let limited = StorageReadControl::with_limit(size_of::<HNSWIndex>() - 1);
    let rejected = HNSWIndex::from_persistence_controlled(
        4,
        source.params(),
        snapshot.meta,
        snapshot.nodes,
        &limited,
    );
    assert!(matches!(
        rejected,
        Err(crate::StorageBackendError::Memory(_))
    ));
    assert_eq!(limited.memory().used(), 0);

    let mut snapshot = source.persistence_snapshot();
    let node = snapshot
        .nodes
        .iter_mut()
        .find(|node| node.node_id == node_id)
        .unwrap();
    node.raw_vector.reserve_exact(4096);
    node.neighbors.reserve_exact(64);
    for layer in &mut node.neighbors {
        layer.reserve_exact(128);
    }
    let restored = HNSWIndex::from_persistence_controlled(
        4,
        source.params(),
        snapshot.meta,
        snapshot.nodes,
        &control,
    )
    .unwrap();
    assert!(control.memory().used() > compact_bytes);
    assert_eq!(
        restored.raw_vector(node_id).unwrap().values.capacity(),
        raw_capacity
    );
    assert_eq!(
        restored.persistence_snapshot(),
        source.persistence_snapshot()
    );
    drop(restored);
    assert_eq!(control.memory().used(), 0);
}
