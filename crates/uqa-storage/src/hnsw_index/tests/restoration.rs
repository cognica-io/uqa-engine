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
