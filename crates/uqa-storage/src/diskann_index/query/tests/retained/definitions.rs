//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::vector_index::{SelectedVectorRead, VectorRead, VectorReadSnapshot};
use uqa_core::memory::BudgetedVec;

struct Probe {
    values: VectorReadSnapshot,
    reads: Arc<AtomicUsize>,
}

impl VectorRead for Probe {
    fn check_control(&self, control: &StorageReadControl) -> StorageBackendResult<()> {
        self.values.check_control(control)
    }
    fn dimensions(&self) -> u32 {
        self.values.dimensions()
    }
    fn next_document_after(
        &self,
        after: Option<DocId>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<DocId>> {
        self.values.next_document_after(after, control)
    }
    fn document_vector_count(
        &self,
        document: DocId,
        control: &StorageReadControl,
    ) -> StorageBackendResult<u64> {
        self.values.document_vector_count(document, control)
    }
    fn read_vector(
        &self,
        document: DocId,
        ordinal: u32,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<BudgetedVec<f32>>> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        self.values.read_vector(document, ordinal, control)
    }
}

fn raw(
    index: &MemoryVectorIndex,
    control: &StorageReadControl,
) -> (VectorReadSnapshot, Arc<AtomicUsize>) {
    let values = index
        .snapshot_with_control(control)
        .unwrap()
        .vector_read_snapshot(control)
        .unwrap()
        .unwrap();
    let reads = Arc::new(AtomicUsize::new(0));
    (
        Arc::new(Probe {
            values,
            reads: reads.clone(),
        }),
        reads,
    )
}

fn exact(source: &Source) -> MemoryVectorIndex {
    let mut index = MemoryVectorIndex::new(2);
    for (&document, (_, values)) in &source.documents {
        index.add_many(document, values.clone()).unwrap();
    }
    index
}

#[test]
fn diskann_definition_values_reuse_graph_candidates_without_capture_time_reads() {
    let source = Source::new((1..=16).map(|document| {
        (
            document,
            vec![vec![
                (document * 37 % 101) as f32 - 50.0,
                (document * 61 % 97) as f32 - 48.0,
            ]],
        )
    }));
    let exact = exact(&source);
    let physical = Observed::new(&source);
    let owner = StorageReadControl::with_limit(1 << 20);
    let index = RetainedDiskANNIndex::open(
        source.clone(),
        physical.clone(),
        parameters(),
        limits(),
        &owner,
    )
    .unwrap();
    source.reset();
    let records = physical.records.load(Ordering::Relaxed);
    let (values, reads) = raw(&exact, &owner);
    let retained = index
        .snapshot_with_vector_read(values, &owner)
        .unwrap()
        .unwrap();
    assert_eq!(retained.index_kind(), "diskann");
    assert_eq!(retained.count().unwrap(), 16);
    assert!(retained.contains_document(16).unwrap());
    assert!(!retained.contains_document(17).unwrap());
    assert!(retained.diskann_read_snapshot(&owner).unwrap().is_none());
    assert_eq!(reads.load(Ordering::Relaxed), 0);
    assert!(source.visits.lock().unwrap().is_empty());
    assert_eq!(physical.records.load(Ordering::Relaxed), records);
    assert_eq!(physical.pages.load(Ordering::Relaxed), 0);
    let mut approximate = false;
    for vectors in exact.vectors().values() {
        let query = &vectors[0];
        for k in [1, 3, 20] {
            let expected = index.search_knn(query, k).unwrap();
            assert_eq!(
                bits(&retained.search_knn(query, k).unwrap()),
                bits(&expected)
            );
            approximate |= bits(&expected) != bits(&exact.search_knn(query, k).unwrap());
        }
    }
    assert!(
        approximate,
        "fixture must distinguish physical ANN from a disguised exact scan"
    );
    assert!(physical.pages.load(Ordering::Relaxed) > 0);
    assert!(reads.load(Ordering::Relaxed) > 0);
    drop((retained, index));
    assert_eq!(owner.memory().used(), 0);
}

#[test]
fn diskann_definition_raw_capability_keeps_the_projected_owner_control() {
    let actual = Source::new([(1, vec![vec![1.0, 0.0]])]);
    let physical = Observed::new(&actual);
    let owner = StorageReadControl::with_limit(1 << 20);
    let source_owner = StorageReadControl::with_limit(1 << 20);
    let (values, _) = raw(&exact(&actual), &source_owner);
    let index =
        RetainedDiskANNIndex::open(actual, physical, parameters(), limits(), &owner).unwrap();
    let retained = index
        .snapshot_with_vector_read(values, &owner)
        .unwrap()
        .unwrap();
    let source = retained.vector_read_snapshot(&owner).unwrap().unwrap();
    drop((index, retained));
    assert_eq!(source.document_vector_count(1, &source_owner).unwrap(), 1);
    owner.cancellation().cancel();
    assert!(source.document_vector_count(1, &source_owner).is_err());
    assert!(source.read_vector(1, 0, &source_owner).is_err());
    drop(source);
    assert_eq!(owner.memory().used(), 0);
    assert_eq!(source_owner.memory().used(), 0);
}

#[test]
fn diskann_definition_values_merge_complete_tensor_differences_and_mask_later_rows() {
    let actual = Source::new([
        (1, vec![vec![0.0, 1.0], vec![-1.0, 0.0]]),
        (2, vec![vec![1.0, 0.0]]),
        (3, vec![vec![-1.0, 0.0]]),
        (4, vec![vec![0.0, -0.0]]),
    ]);
    let fixed = Source::new([
        (1, vec![vec![0.0, 1.0], vec![1.0, 0.0]]),
        (3, vec![vec![0.0, 1.0]]),
        (4, vec![vec![0.0, 0.0]]),
        (5, vec![vec![1.0, 0.0]]),
    ]);
    let exact = exact(&fixed);
    let physical = Observed::new(&actual);
    let owner = StorageReadControl::with_limit(1 << 20);
    let index =
        RetainedDiskANNIndex::open(actual, physical, parameters(), limits(), &owner).unwrap();
    let (values, _) = raw(&exact, &owner);
    let retained = index
        .snapshot_with_vector_read(values, &owner)
        .unwrap()
        .unwrap();
    assert_eq!(retained.count().unwrap(), 5);
    assert!(!retained.contains_document(2).unwrap());
    assert!(retained.contains_document(5).unwrap());
    for query in [
        [1.0, 0.0],
        [0.0, 0.0],
        [f32::from_bits(1), 0.0],
        [f32::MAX, f32::MAX],
    ] {
        for k in 0..=6 {
            assert_eq!(
                bits(&retained.search_knn(&query, k).unwrap()),
                bits(&exact.search_knn(&query, k).unwrap())
            );
        }
        for threshold in [-1.0, 0.0, 0.5, 1.0] {
            assert_eq!(
                bits(&retained.search_threshold(&query, threshold).unwrap()),
                bits(&exact.search_threshold(&query, threshold).unwrap())
            );
        }
    }
    assert!(retained.search_knn(&[1.0], 0).is_err());
    assert!(retained.search_threshold(&[1.0, 0.0], f32::NAN).is_err());
    let (replacement, _) = raw(&MemoryVectorIndex::new(2), &owner);
    let overlay = SelectedVectorRead::capture(
        retained.vector_read_snapshot(&owner).unwrap(),
        2,
        [(1, None), (3, Some(replacement))],
        &owner,
    )
    .unwrap();
    let nested = retained
        .snapshot_with_vector_read(overlay, &owner)
        .unwrap()
        .unwrap();
    drop((retained, index));
    assert_eq!(
        bits(&nested.search_knn(&[1.0, 0.0], 10).unwrap()),
        [(4, 0.0_f64.to_bits()), (5, 1.0_f64.to_bits())]
    );
    assert_eq!(nested.count().unwrap(), 2);
    drop(nested);
    assert_eq!(owner.memory().used(), 0);
}

#[test]
fn diskann_definition_values_preserve_original_quotas_and_independent_cancellation() {
    let actual = Source::new([(1, vec![vec![1.0, 0.0]])]);
    let physical = Observed::new(&actual);
    let owner = StorageReadControl::with_limit(1 << 20);
    let source_owner = StorageReadControl::with_limit(1 << 20);
    let index = RetainedDiskANNIndex::open(
        actual.clone(),
        physical.clone(),
        parameters(),
        limits(),
        &owner,
    )
    .unwrap();
    let (values, reads) = raw(&exact(&actual), &source_owner);
    let source_lease = Arc::downgrade(&values);
    let retained = index
        .snapshot_with_vector_read(values, &owner)
        .unwrap()
        .unwrap();
    let fresh = StorageReadControl::with_limit(1 << 20);
    let held = owner.memory().used();
    let reservation = owner
        .memory()
        .reserve(owner.memory().limit() - held)
        .unwrap();
    assert!(retained
        .search_knn_with_control(&[1.0, 0.0], 1, &fresh)
        .is_err());
    assert_eq!(fresh.memory().used(), 0);
    assert!(retained
        .search_knn_with_control(&[1.0, 0.0], 0, &fresh)
        .unwrap()
        .is_empty());
    drop(reservation);
    *physical.cancel.lock().unwrap() = Some(fresh.cancellation().clone());
    assert!(retained
        .search_knn_with_control(&[1.0, 0.0], 1, &fresh)
        .is_err());
    assert_eq!(
        reads.load(Ordering::Relaxed),
        0,
        "page failure must not run an exact recovery scan"
    );
    assert_eq!(owner.memory().used(), held);
    source_owner.cancellation().cancel();
    assert!(retained.count().is_err());
    assert!(retained.search_knn(&[1.0, 0.0], 0).is_err());
    assert!(retained.vector_read_snapshot(&owner).is_err());
    drop((index, retained));
    assert!(source_lease.upgrade().is_none());
    assert_eq!(owner.memory().used(), 0);
    assert_eq!(source_owner.memory().used(), 0);
}
