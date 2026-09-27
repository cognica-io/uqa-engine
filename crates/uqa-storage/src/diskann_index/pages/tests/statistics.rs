//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn diskann_page_work_uses_actual_cache_leases_and_keeps_concurrent_invocations_separate() {
    let fixture = fixture(2, 40, 0);
    let physical = MemoryBudget::new(1 << 20);
    let owner = StorageReadControl::with_limit(1 << 20);
    let source = Arc::new(Counted::new(fixture.memory(&physical, &owner).unwrap()));
    let reader = fixture
        .reader(source.clone(), limits(PAGE_BYTES * 2), &owner)
        .unwrap();
    let query = StorageReadControl::with_limit(65_536);
    let (nodes, cold) = reader
        .read_nodes_with_stats(&[39, 2, 31, 0], &query)
        .unwrap();
    assert_eq!(
        cold,
        DiskANNPageReadStats {
            page_requests: 1,
            cache_hits: 0,
            provider_pages: 1,
            provider_batches: 1
        }
    );
    assert_eq!(
        nodes.iter().map(DiskANNNode::node_id).collect::<Vec<_>>(),
        [39, 2, 31, 0]
    );
    drop(nodes);
    let expected = DiskANNPageReadStats {
        page_requests: 1,
        cache_hits: 1,
        provider_pages: 0,
        provider_batches: 0,
    };
    std::thread::scope(|scope| {
        let first = scope.spawn(|| reader.read_nodes_with_stats(&[2], &query).unwrap().1);
        let second = scope.spawn(|| reader.read_nodes_with_stats(&[31], &query).unwrap().1);
        assert_eq!(first.join().unwrap(), expected);
        assert_eq!(second.join().unwrap(), expected);
    });
    assert_eq!(source.graph_reads.load(Ordering::Relaxed), 1);
    assert_eq!(query.memory().used(), 0);
    let (empty, stats) = reader.read_nodes_with_stats(&[], &query).unwrap();
    assert!(empty.is_empty());
    assert_eq!(stats, DiskANNPageReadStats::default());
    query.cancellation().cancel();
    assert!(reader.read_nodes_with_stats(&[0], &query).is_err());
    assert_eq!(source.graph_reads.load(Ordering::Relaxed), 1);
    assert_eq!(query.memory().used(), 0);
}

#[test]
fn diskann_page_work_counts_fragment_requests_and_actual_provider_batches() {
    let fixture = fixture(1024, 3, 0);
    let physical = MemoryBudget::new(1 << 20);
    let owner = StorageReadControl::with_limit(1 << 20);
    let source = Arc::new(Counted::new(fixture.memory(&physical, &owner).unwrap()));
    let reader = fixture.reader(source.clone(), limits(0), &owner).unwrap();
    let query = StorageReadControl::with_limit(65_536);
    let (nodes, stats) = reader.read_nodes_with_stats(&[2, 0, 1], &query).unwrap();
    assert_eq!(
        stats,
        DiskANNPageReadStats {
            page_requests: 6,
            cache_hits: 0,
            provider_pages: 6,
            provider_batches: 3
        }
    );
    assert_eq!(source.graph_reads.load(Ordering::Relaxed), 6);
    assert_eq!(source.graph_batches.load(Ordering::Relaxed), 3);
    assert_eq!(source.largest_batch.load(Ordering::Relaxed), 2);
    drop(nodes);
    assert_eq!(query.memory().used(), 0);
    let zero = StorageReadControl::with_limit(0);
    assert!(reader.read_nodes_with_stats(&[0], &zero).is_err());
    assert_eq!(source.graph_reads.load(Ordering::Relaxed), 6);
    assert_eq!(source.graph_batches.load(Ordering::Relaxed), 3);
    assert_eq!(zero.memory().used(), 0);
}

#[test]
fn diskann_page_work_rejects_overflow_without_partially_updating_counts() {
    let mut stats = DiskANNPageReadStats {
        provider_pages: u64::MAX,
        ..DiskANNPageReadStats::default()
    };
    let before = stats;
    assert!(stats
        .merge(DiskANNPageReadStats {
            page_requests: 1,
            provider_pages: 1,
            provider_batches: 1,
            cache_hits: 0
        })
        .is_err());
    assert_eq!(stats, before);
}
