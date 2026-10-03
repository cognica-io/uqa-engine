//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    hnsw_index::{HNSWNodeSnapshot, HNSWRestoreBuilder},
    VectorIndex,
};

#[test]
fn canonical_streams_require_every_identity_and_coordinate_bit() {
    let control = StorageReadControl::with_limit(64 * 1024);
    let mut index = HNSWIndex::new(2);
    index.add(1, vec![1.0, -0.0]).unwrap();
    index.add(2, vec![0.0, 1.0]).unwrap();
    let mut valid = HNSWCanonicalValidator::new(&index, &control);
    valid.push(1, 0, &[1.0, -0.0]).unwrap();
    valid.push(2, 0, &[0.0, 1.0]).unwrap();
    valid.finish().unwrap();
    for (document, ordinal, vector, message) in [
        (0, 0, [1.0, -0.0], "has no live graph node"),
        (2, 0, [0.0, 1.0], "has no canonical vector"),
        (1, 0, [1.0, 0.0], "differs from its live graph node"),
    ] {
        let mut check = HNSWCanonicalValidator::new(&index, &control);
        assert!(check
            .push(document, ordinal, &vector)
            .unwrap_err()
            .to_string()
            .contains(message));
        assert!(check.finish().is_err());
    }
    let mut truncated = HNSWCanonicalValidator::new(&index, &control);
    truncated.push(1, 0, &[1.0, -0.0]).unwrap();
    assert!(truncated
        .finish()
        .unwrap_err()
        .to_string()
        .contains("has no canonical vector"));
    let mut repeated = HNSWCanonicalValidator::new(&index, &control);
    repeated.push(1, 0, &[1.0, -0.0]).unwrap();
    assert!(repeated.push(1, 0, &[1.0, -0.0]).is_err());
    let cancelled = HNSWCanonicalValidator::new(&index, &control);
    control.cancellation().cancel();
    assert!(cancelled.finish().is_err());
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn streamed_edge_growth_charges_capacity_and_rejects_incomplete_publication() {
    let control = StorageReadControl::with_limit(64 * 1024);
    let mut source = HNSWIndex::new(2);
    source.add(1, vec![1.0, 0.0]).unwrap();
    source.add(2, vec![0.0, 1.0]).unwrap();
    let delta = source.take_persistence_delta();
    let mut builder = HNSWRestoreBuilder::new(2, source.params(), delta.meta, &control).unwrap();
    for node in delta.nodes() {
        let node = node.unwrap();
        builder
            .push(HNSWNodeSnapshot {
                neighbors: vec![Vec::new(); node.level + 1],
                ..(*node).clone()
            })
            .unwrap();
    }
    let before = control.memory().used();
    builder.edge(1, 0, 2).unwrap();
    assert!(control.memory().used() > before);
    let held = control
        .memory()
        .reserve(control.memory().limit() - control.memory().used())
        .unwrap();
    assert!(matches!(
        builder.edge(2, 0, 1),
        Err(StorageBackendError::Memory(_))
    ));
    drop(held);
    assert!(builder.finish().is_err());
    assert_eq!(control.memory().used(), 0);
    source.validate_invariants().unwrap();
}
