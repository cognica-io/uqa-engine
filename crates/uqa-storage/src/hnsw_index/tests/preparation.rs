//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Candidate preparation preserves graph topology, compaction order and caller resource ownership.

use super::{vector, HNSWIndex, HNSWIndexParams, VectorIndex};
use crate::{hnsw_index::HNSWMutation, read_control::StorageReadControl, StorageBackendError};
use uqa_core::memory::MemoryError;

fn source() -> HNSWIndex {
    let mut index = HNSWIndex::with_params(
        4,
        HNSWIndexParams {
            m: 4,
            ef_construction: 8,
            rebuild_threshold: 3,
            ..HNSWIndexParams::default()
        },
    )
    .unwrap();
    for document in 1..=24 {
        index.add(document, vector(document, 4)).unwrap();
    }
    index.take_persistence_delta();
    index
}

#[test]
fn controlled_capture_preserves_graph_topology_tombstones_and_pending_persistence() {
    let mut source = source();
    source
        .add_many(2, vec![vector(90, 4), vector(91, 4)])
        .unwrap();
    source.delete(1).unwrap();
    let control = StorageReadControl::with_limit(1 << 20);
    let snapshot = source.snapshot_controlled(&control).unwrap();
    assert_eq!(
        snapshot.persistence_snapshot(),
        source.persistence_snapshot()
    );
    assert_eq!(snapshot.dirty_nodes, source.dirty_nodes);
    assert_eq!(snapshot.full_rewrite, source.full_rewrite);
    snapshot.validate_invariants().unwrap();
    let before = snapshot.persistence_snapshot();
    source.clear().unwrap();
    drop(source);
    assert_eq!(snapshot.persistence_snapshot(), before);
    assert_eq!(control.memory().used(), snapshot.reserved_bytes());
    drop(snapshot);
    assert_eq!(control.memory().used(), 0);
}

fn apply(index: &mut HNSWIndex, mutation: HNSWMutation<'_>) {
    match mutation {
        HNSWMutation::Replace { document, vectors } => {
            index.add_many(document, vectors.to_vec()).unwrap();
        }
        HNSWMutation::Delete(document) => index.delete(document).unwrap(),
        HNSWMutation::Clear => index.clear().unwrap(),
    }
}

#[test]
fn prepared_graph_deltas_match_ordered_mutations_and_compaction_without_changing_the_source() {
    let source = source();
    let original = source.persistence_snapshot();
    let vectors = [vector(99, 4), vector(100, 4)];
    let changes = [
        HNSWMutation::Replace {
            document: 2,
            vectors: &vectors,
        },
        HNSWMutation::Delete(1),
        HNSWMutation::Replace {
            document: 2,
            vectors: &[],
        },
        HNSWMutation::Replace {
            document: 30,
            vectors: &vectors,
        },
        HNSWMutation::Delete(999),
        HNSWMutation::Clear,
        HNSWMutation::Replace {
            document: 31,
            vectors: &vectors,
        },
    ];
    let mut expected = source.clone();
    for count in 1..=changes.len() {
        apply(&mut expected, changes[count - 1]);
        expected.validate_invariants().unwrap();
        let control = StorageReadControl::with_limit(1 << 20);
        let prepared = source
            .prepare_delta_changes(&changes[..count], &control)
            .unwrap();
        assert_eq!(*prepared, expected.clone().take_persistence_delta());
        assert_eq!(source.persistence_snapshot(), original);
        assert!(source.dirty_nodes.is_empty());
        assert!(!source.full_rewrite);
        assert_eq!(control.memory().used(), prepared.reserved_bytes());
        drop(prepared);
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn candidate_limits_fail_before_graph_cloning_and_release_every_reservation() {
    let source = source();
    let original = source.persistence_snapshot();
    let vectors = [vector(99, 4)];
    let mutation = HNSWMutation::Replace {
        document: 2,
        vectors: &vectors,
    };
    let limited = StorageReadControl::with_limit(1);
    let allocation = allocation_counter::measure(|| {
        assert!(matches!(
            source.prepare_delta(mutation, &limited),
            Err(StorageBackendError::Memory(MemoryError::Limit { .. }))
        ));
    });
    assert_eq!(allocation.bytes_total, 0);
    assert_eq!(limited.memory().used(), 0);
    let cleared = source.prepare_delta(HNSWMutation::Clear, &limited).unwrap();
    assert!(cleared.full_rewrite);
    assert!(cleared.nodes.is_empty());
    assert_eq!(limited.memory().used(), 0);
    let control = StorageReadControl::with_limit(1 << 20);
    let delta = source.prepare_delta(mutation, &control).unwrap();
    let peak = control.memory().peak();
    assert!(peak > delta.reserved_bytes());
    drop(delta);
    let late_limit = StorageReadControl::with_limit(peak - 1);
    assert!(matches!(
        source.prepare_delta(mutation, &late_limit),
        Err(StorageBackendError::Memory(MemoryError::Limit { .. }))
    ));
    assert_eq!(late_limit.memory().used(), 0);
    control.cancellation().cancel();
    assert!(matches!(
        source.prepare_delta(mutation, &control),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert_eq!(control.memory().used(), 0);
    assert_eq!(source.persistence_snapshot(), original);
}

#[test]
fn canonical_preparation_matches_serial_topology_and_rejects_incomplete_tensors() {
    let params = HNSWIndexParams::default();
    let vectors = vec![
        (1, 0, vector(1, 4)),
        (1, 1, vector(2, 4)),
        (2, 0, vector(3, 4)),
    ];
    let mut expected = HNSWIndex::with_params(4, params).unwrap();
    expected
        .add_many(1, vec![vector(1, 4), vector(2, 4)])
        .unwrap();
    expected.add(2, vector(3, 4)).unwrap();
    let control = StorageReadControl::with_limit(1 << 20);
    let prepared = HNSWIndex::prepare_canonical(4, params, &vectors, &control).unwrap();
    assert_eq!(*prepared, expected.take_persistence_delta());
    assert_eq!(control.memory().used(), prepared.reserved_bytes());
    drop(prepared);
    for identities in [[(1, 0), (1, 2)], [(1, 1), (2, 0)], [(2, 0), (1, 0)]] {
        let invalid = identities
            .into_iter()
            .map(|(document, ordinal)| (document, ordinal, vector(document, 4)))
            .collect::<Vec<_>>();
        assert!(HNSWIndex::prepare_canonical(4, params, &invalid, &control).is_err());
        assert_eq!(control.memory().used(), 0);
    }
    let limited = StorageReadControl::with_limit(1);
    let allocation = allocation_counter::measure(|| {
        assert!(matches!(
            HNSWIndex::prepare_canonical(4, params, &vectors, &limited),
            Err(StorageBackendError::Memory(MemoryError::Limit { .. }))
        ));
    });
    assert_eq!(allocation.bytes_total, 0);
    assert_eq!(limited.memory().used(), 0);
}

#[test]
fn ordered_preparation_allows_compaction_to_reset_an_exhausted_node_allocator() {
    let mut source = source();
    source.params.rebuild_threshold = 1;
    source.next_node_id = u64::MAX;
    let before = source.persistence_snapshot();
    let vectors = [vector(99, 4), vector(100, 4)];
    let replacement = HNSWMutation::Replace {
        document: 30,
        vectors: &vectors,
    };
    let control = StorageReadControl::with_limit(1 << 20);
    assert!(source.prepare_delta(replacement, &control).is_err());
    assert_eq!(control.memory().used(), 0);
    let changes = [HNSWMutation::Delete(1), replacement];
    let prepared = source.prepare_delta_changes(&changes, &control).unwrap();
    let mut expected = source.clone();
    for mutation in changes {
        apply(&mut expected, mutation);
    }
    assert_eq!(*prepared, expected.take_persistence_delta());
    assert_eq!(source.persistence_snapshot(), before);
}

#[test]
fn neighbor_selection_observes_cancellation_after_evaluation_starts() {
    let source = source();
    let control = StorageReadControl::with_limit(1 << 20);
    let candidates = source.nodes.keys().copied().inspect(|_| {
        control.cancellation().cancel();
    });
    assert!(matches!(
        source.select_neighbors(&vector(3, 4), candidates, 4, None, Some(&control)),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert_eq!(control.memory().used(), 0);
}
