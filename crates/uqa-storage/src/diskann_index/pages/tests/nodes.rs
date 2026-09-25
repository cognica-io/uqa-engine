//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn batched_nodes_share_packed_pages_and_preserve_priority_without_a_cache() {
    let fixture = fixture(2, 40, 0);
    let physical = MemoryBudget::new(1 << 20);
    let owner = StorageReadControl::with_limit(1 << 20);
    let source = Arc::new(Counted::new(fixture.memory(&physical, &owner).unwrap()));
    let reader = fixture.reader(source.clone(), limits(0), &owner).unwrap();
    let query = StorageReadControl::with_limit(65_536);
    let ids = [39, 2, 31, 0];
    let nodes = reader.read_nodes(&ids, &query).unwrap();
    assert_eq!(
        nodes.iter().map(DiskANNNode::node_id).collect::<Vec<_>>(),
        ids
    );
    assert_eq!(source.graph_reads.load(Ordering::Relaxed), 1);
    assert_eq!(reader.cache_bytes(), 0);
    for node in nodes.iter() {
        assert_eq!(node.doc_id(), node.node_id() + 10);
        assert_eq!(node.version(), version());
    }
    drop(nodes);
    assert_eq!(query.memory().used(), 0);
    for ids in [&[2, 2][..], &[40], &[u64::MAX]] {
        assert!(reader.read_nodes(ids, &query).is_err());
        assert_eq!(source.graph_reads.load(Ordering::Relaxed), 1);
        assert_eq!(query.memory().used(), 0);
    }
    assert!(reader.read_nodes(&[], &query).unwrap().is_empty());
    assert_eq!(source.graph_reads.load(Ordering::Relaxed), 1);
}

#[test]
fn batched_fragment_assembly_respects_in_flight_pages_and_original_allowance() {
    let fixture = fixture(1024, 3, 0);
    let physical = MemoryBudget::new(1 << 20);
    let owner = StorageReadControl::with_limit(1 << 20);
    let source = Arc::new(Counted::new(fixture.memory(&physical, &owner).unwrap()));
    let reader = fixture.reader(source.clone(), limits(0), &owner).unwrap();
    let mut rejected = false;
    let mut accepted = false;
    for bytes in [0, 1024, 4096, 8192, 16_384, 32_768, 65_536] {
        let query = StorageReadControl::with_limit(bytes);
        match reader.read_nodes(&[2, 0, 1], &query) {
            Ok(nodes) => {
                accepted = true;
                assert_eq!(
                    nodes.iter().map(DiskANNNode::node_id).collect::<Vec<_>>(),
                    [2, 0, 1]
                );
                for node in nodes.iter() {
                    assert_eq!(node.vector().len(), 1024);
                    assert_eq!(
                        node.vector()[0],
                        if node.node_id() % 2 == 0 { 1.0 } else { -1.0 }
                    );
                }
            }
            Err(StorageBackendError::Memory(_)) => rejected = true,
            Err(error) => panic!("unexpected error: {error}"),
        }
        assert_eq!(query.memory().used(), 0);
    }
    assert!(accepted && rejected);
    assert_eq!(source.largest_batch.load(Ordering::Relaxed), 2);
    let query = StorageReadControl::with_limit(65_536);
    query.cancellation().cancel();
    let before = source.graph_reads.load(Ordering::Relaxed);
    assert!(matches!(
        reader.read_nodes(&[0, 1], &query),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert_eq!(source.graph_reads.load(Ordering::Relaxed), before);
    assert_eq!(query.memory().used(), 0);
}
