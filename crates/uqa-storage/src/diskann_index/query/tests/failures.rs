//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::diskann_index::pages::{
    DiskANNPageVisitor, DiskANNReadCapabilities, DiskANNRecordKey, DiskANNRecordVisitor,
};
use std::sync::atomic::AtomicU8;

struct Faults {
    memory: Arc<DiskANNMemorySource>,
    fault: AtomicU8,
}

impl DiskANNPageSource for Faults {
    fn generation(&self) -> DiskANNGeneration {
        self.memory.generation()
    }
    fn capabilities(&self) -> DiskANNReadCapabilities {
        self.memory.capabilities()
    }
    fn read_record(
        &self,
        key: DiskANNRecordKey,
        limit: usize,
        control: &StorageReadControl,
        visit: &mut DiskANNRecordVisitor<'_>,
    ) -> StorageBackendResult<()> {
        self.memory.read_record(key, limit, control, visit)
    }
    fn read_graph_pages(
        &self,
        pages: &[u64],
        control: &StorageReadControl,
        visit: &mut DiskANNPageVisitor<'_>,
    ) -> StorageBackendResult<()> {
        match self.fault.load(Ordering::Relaxed) {
            1 => Ok(()),
            2 => Err(uqa_core::memory::MemoryError::Limit {
                required: 42,
                limit: 17,
            }
            .into()),
            3 => {
                control.cancellation().cancel();
                self.memory.read_graph_pages(pages, control, visit)
            }
            _ => self.memory.read_graph_pages(pages, control, visit),
        }
    }
}

#[test]
fn diskann_document_query_propagates_page_failures_and_releases_partial_workspace() {
    let base = Source::new([(1, vec![vec![1.0, 0.0]]), (2, vec![vec![0.0, 0.0]])]);
    let physical = Arc::new(Faults {
        memory: build(&base),
        fault: AtomicU8::new(0),
    });
    let owner = StorageReadControl::with_limit(1 << 20);
    let query =
        DiskANNQuery::open(&base, physical.clone(), parameters(), limits(), &owner).unwrap();
    base.reset();
    for fault in 1..=3 {
        physical.fault.store(fault, Ordering::Relaxed);
        let control = StorageReadControl::with_limit(65_536);
        assert!(query.search_knn(&[1.0, 0.0], 2, &control).is_err());
        assert_eq!(control.memory().used(), 0);
        assert_eq!(
            base.scans.load(Ordering::Relaxed),
            0,
            "failed ANN never recovers by scanning canonical data"
        );
    }
    // Declared exact routes do not touch graph pages, even when a normal ANN read would fail.
    let control = StorageReadControl::with_limit(65_536);
    assert_eq!(
        ids(&query.search_knn(&[0.0, 0.0], 2, &control).unwrap().postings),
        [1, 2]
    );
    assert_eq!(
        ids(&query.search_threshold(&[1.0, 0.0], 0.0, &control).unwrap()),
        [1, 2]
    );
    physical.fault.store(0, Ordering::Relaxed);
    assert_eq!(
        ids(&query.search_knn(&[1.0, 0.0], 2, &control).unwrap().postings),
        [1, 2]
    );
}

#[test]
fn diskann_document_query_checks_original_controls_zero_k_and_numeric_validation() {
    let base = Source::new([(1, vec![vec![1.0, 0.0]])]);
    let physical = build(&base);
    let owner = StorageReadControl::with_limit(1 << 20);
    let query = DiskANNQuery::open(&base, physical, parameters(), limits(), &owner).unwrap();
    let tiny = StorageReadControl::with_limit(1);
    assert!(query.search_knn(&[1.0, 0.0], 1, &tiny).is_err());
    assert_eq!(tiny.memory().used(), 0);
    assert!(query
        .search_knn(&[1.0, 0.0], 0, &tiny)
        .unwrap()
        .postings
        .is_empty());
    assert!(query.search_knn(&[1.0], 0, &tiny).is_err());
    assert!(query.search_knn(&[f32::NAN, 0.0], 0, &tiny).is_err());
    assert!(query
        .search_threshold(&[1.0, 0.0], f32::NAN, &tiny)
        .is_err());
    owner.cancellation().cancel();
    assert!(query.search_knn(&[1.0, 0.0], 0, &tiny).is_err());
    assert!(query.search_threshold(&[1.0, 0.0], 0.0, &tiny).is_err());
}

#[test]
fn diskann_document_query_rejects_equal_version_with_a_different_canonical_tensor_shape() {
    let base = Source::new([(1, vec![vec![1.0, 0.0]])]);
    let physical = build(&base);
    let mut corrupt = base.clone();
    corrupt
        .documents
        .get_mut(&1)
        .unwrap()
        .1
        .push(vec![0.0, 1.0]);
    let owner = StorageReadControl::with_limit(1 << 20);
    let query = DiskANNQuery::open(&corrupt, physical, parameters(), limits(), &owner).unwrap();
    let control = StorageReadControl::with_limit(65_536);
    assert!(query.search_knn(&[1.0, 0.0], 1, &control).is_err());
    assert_eq!(control.memory().used(), 0);
}
