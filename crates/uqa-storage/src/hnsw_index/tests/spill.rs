//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    hnsw_index::{HNSWCanonicalBuilder, HNSWRestoreBuilder},
    read_control::StorageReadControl,
};

#[test]
fn graph_larger_than_its_allowance_keeps_topology_scores_and_retained_readers() {
    let control = StorageReadControl::with_limit(64 * 1024);
    let dimensions = 1024;
    let count = 20;
    assert!(count * dimensions * size_of::<f32>() > control.memory().limit());
    let params = HNSWIndexParams {
        m: 2,
        ef_construction: 4,
        ef_search: 12,
        rebuild_threshold: 3,
        ..HNSWIndexParams::default()
    };
    let mut resident = HNSWIndex::with_params(dimensions as u32, params).unwrap();
    let mut builder = HNSWCanonicalBuilder::new(dimensions as u32, params, &control).unwrap();
    for document in 1..=count as u64 {
        let value = vector(document, dimensions);
        resident.add(document, value.clone()).unwrap();
        builder.push(document, 0, &value).unwrap();
    }
    let (mut spilled, memory) = builder.finish().unwrap().into_parts();
    assert!(spilled.nodes.is_spilled());
    spilled.validate_invariants().unwrap();
    let expected = resident.take_persistence_delta();
    let actual = spilled.take_persistence_delta();
    assert_eq!(actual.meta, expected.meta);
    let mut nodes = actual.nodes();
    for node in expected.nodes() {
        assert_eq!(*node.unwrap(), *nodes.next().unwrap().unwrap());
    }
    assert!(nodes.next().is_none());
    drop(nodes);
    for query in [3, 15, 90] {
        assert_eq!(
            spilled.search_knn(&vector(query, dimensions), 7).unwrap(),
            resident.search_knn(&vector(query, dimensions), 7).unwrap()
        );
    }

    let retained = spilled.snapshot_with_control(&control).unwrap();
    let query = vector(17, dimensions);
    let old_results = retained.search_knn(&query, 7).unwrap();
    spilled.add(17, vector(250, dimensions)).unwrap();
    spilled.delete(8).unwrap();
    assert_eq!(retained.search_knn(&query, 7).unwrap(), old_results);
    assert_eq!(retained.count().unwrap(), count);
    assert_eq!(spilled.count().unwrap(), count - 1);
    let changed = spilled.take_persistence_delta();
    assert!(!changed.full_rewrite);
    assert!(changed.nodes().map(|node| node.unwrap()).count() > 0);
    spilled.delete(9).unwrap();
    resident.add(17, vector(250, dimensions)).unwrap();
    resident.delete(8).unwrap();
    resident.delete(9).unwrap();
    let compacted = spilled.take_persistence_delta();
    let reference = resident.take_persistence_delta();
    assert!(compacted.full_rewrite);
    assert_eq!(compacted.meta, reference.meta);
    let mut nodes = compacted.nodes();
    for expected in reference.nodes() {
        assert_eq!(*nodes.next().unwrap().unwrap(), *expected.unwrap());
    }
    assert!(nodes.next().is_none());
    drop(nodes);
    assert_eq!(retained.search_knn(&query, 7).unwrap(), old_results);
    assert_eq!(
        spilled.search_knn(&query, 7).unwrap(),
        resident.search_knn(&query, 7).unwrap()
    );
    assert!(control.memory().peak() <= control.memory().limit());
    drop((retained, changed, actual, compacted, spilled, memory));
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn restoration_streams_nodes_and_preserves_bits_with_more_vectors_than_memory() {
    let control = StorageReadControl::with_limit(64 * 1024);
    let params = HNSWIndexParams {
        m: 2,
        ef_construction: 4,
        ..HNSWIndexParams::default()
    };
    let mut source = HNSWIndex::with_params(1024, params).unwrap();
    for document in 1..=20 {
        source.add(document, vector(document, 1024)).unwrap();
    }
    let delta = source.take_persistence_delta();
    let mut builder = HNSWRestoreBuilder::new(1024, params, delta.meta, &control).unwrap();
    for node in delta.nodes() {
        builder.push(node.unwrap().into_parts().0).unwrap();
    }
    let restored = builder.finish().unwrap();
    assert!(restored.nodes.is_spilled());
    let query = vector(90, 1024);
    assert_eq!(
        restored.search_knn(&query, 10).unwrap(),
        source.search_knn(&query, 10).unwrap()
    );
    let actual = restored.delta(&control);
    let mut actual = actual;
    actual.full_rewrite = true;
    let mut nodes = actual.nodes();
    for expected in delta.nodes() {
        assert_eq!(*nodes.next().unwrap().unwrap(), *expected.unwrap());
    }
    assert!(nodes.next().is_none());
    drop(nodes);
    drop((actual, restored));
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn adaptive_tensor_search_spills_candidates_without_allocating_one_result_per_vector() {
    let control = StorageReadControl::with_limit(32 * 1024);
    let params = HNSWIndexParams {
        m: 2,
        ef_construction: 4,
        ef_search: 8,
        ..HNSWIndexParams::default()
    };
    let mut source = HNSWIndex::with_params(2, params).unwrap();
    source.add_many(7, vec![vec![0.75, 0.25]; 80]).unwrap();
    let delta = source.take_persistence_delta();
    let mut builder = HNSWRestoreBuilder::new(2, params, delta.meta, &control).unwrap();
    for node in delta.nodes() {
        builder.push(node.unwrap().into_parts().0).unwrap();
    }
    let restored = builder.finish().unwrap();
    assert!(restored.nodes.is_spilled());
    let query = [0.75, 0.25];
    assert_eq!(
        restored.search_knn(&query, 2).unwrap(),
        source.search_knn(&query, 2).unwrap()
    );
    assert_eq!(
        restored.search_threshold(&query, 0.5).unwrap(),
        source.search_threshold(&query, 0.5).unwrap()
    );
    assert!(control.memory().peak() <= control.memory().limit());
    drop(restored);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn failed_or_cancelled_stream_builders_cannot_publish_partial_graphs() {
    let control = StorageReadControl::with_limit(64 * 1024);
    let params = HNSWIndexParams::default();
    let mut builder = HNSWCanonicalBuilder::new(2, params, &control).unwrap();
    builder.push(1, 0, &[1.0, 0.0]).unwrap();
    assert!(builder.push(1, 2, &[1.0, 0.0]).is_err());
    assert!(builder.push(1, 1, &[1.0, 0.0]).is_err());
    assert!(builder.finish().is_err());
    assert_eq!(control.memory().used(), 0);

    let mut source = HNSWIndex::new(2);
    source.add(1, vec![1.0, 0.0]).unwrap();
    let delta = source.take_persistence_delta();
    let mut restore = HNSWRestoreBuilder::new(2, params, delta.meta, &control).unwrap();
    restore
        .push(delta.nodes().next().unwrap().unwrap().into_parts().0)
        .unwrap();
    assert!(restore.edge(u64::MAX, 0, 1).is_err());
    assert!(restore.finish().is_err());
    assert_eq!(control.memory().used(), 0);

    let builder = HNSWCanonicalBuilder::new(2, params, &control).unwrap();
    control.cancellation().cancel();
    assert!(builder.finish_delta().is_err());
    control.cancellation().reset();
    let restore = HNSWRestoreBuilder::new(2, params, delta.meta, &control).unwrap();
    control.cancellation().cancel();
    assert!(restore.finish().is_err());
    assert_eq!(control.memory().used(), 0);
}
