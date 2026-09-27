//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::sync::atomic::AtomicBool;

struct Fixture {
    physical: Arc<Observed>,
    original: RetainedDiskANNIndex<Source>,
    raw: MemoryVectorIndex,
    control: StorageReadControl,
}

fn fixture() -> Fixture {
    let mut actual = Source::new([
        (1, vec![vec![1.0, 0.0], vec![0.0, 1.0]]),
        (2, vec![vec![-1.0, 0.0]]),
        (3, vec![vec![0.0, 0.0], vec![0.0, 0.0]]),
        (4, vec![]),
        (5, vec![vec![1.0, 0.0]]),
    ]);
    let physical = Observed::new(&actual);
    actual.replace(1, vec![vec![1.0, 0.0], vec![0.0, 1.0]]);
    actual.replace(2, vec![]);
    actual.replace(6, vec![vec![0.0, 1.0]]);
    let mut raw = exact(&actual);
    raw.add(5, vec![-1.0, 0.0]).unwrap();
    raw.add_many(7, vec![vec![1.0, 0.0], vec![0.0, 1.0]])
        .unwrap();
    let control = StorageReadControl::with_limit(1 << 20);
    let original =
        RetainedDiskANNIndex::open(actual, physical.clone(), parameters(), limits(), &control)
            .unwrap();
    Fixture {
        physical,
        original,
        raw,
        control,
    }
}

pub(super) fn counts(index: &dyn VectorIndex) -> Option<(u64, u64)> {
    let control = StorageReadControl::with_limit(0);
    index
        .diskann_query_metadata(&control)
        .unwrap()
        .unwrap()
        .canonical_counts
        .map(|counts| (counts.current_vectors(), counts.changed_vectors()))
}

#[test]
fn diskann_raw_populations_follow_complete_search_without_metadata_reads() {
    let fixture = fixture();
    let (values, reads) = raw(&fixture.raw, &fixture.control);
    let index = fixture
        .original
        .snapshot_with_vector_read(values, &fixture.control)
        .unwrap()
        .unwrap();
    let records = fixture.physical.records.load(Ordering::Relaxed);
    assert_eq!(counts(&*index), None);
    index.search_knn(&[1.0, 0.0], 0).unwrap();
    assert_eq!(counts(&*index), None);
    assert_eq!(reads.load(Ordering::Relaxed), 0);
    assert_eq!(fixture.physical.records.load(Ordering::Relaxed), records);

    index.search_knn(&[0.0, 0.0], 1).unwrap();
    assert_eq!(counts(&*index), None);
    index.search_threshold(&[1.0, 0.0], 1.0).unwrap();
    assert_eq!(counts(&*index), None);

    assert_eq!(
        bits(&index.search_knn(&[1.0, 0.0], 1).unwrap()),
        [(1, 1.0_f64.to_bits())]
    );
    let reads_after = reads.load(Ordering::Relaxed);
    let records_after = fixture.physical.records.load(Ordering::Relaxed);
    let pages_after = fixture.physical.pages.load(Ordering::Relaxed);
    assert_eq!(counts(&*index), Some((8, 6)));
    let nested = index.snapshot_with_control(&fixture.control).unwrap();
    assert_eq!(counts(&*nested), Some((8, 6)));
    assert_eq!(reads.load(Ordering::Relaxed), reads_after);
    assert_eq!(
        fixture.physical.records.load(Ordering::Relaxed),
        records_after
    );
    assert_eq!(fixture.physical.pages.load(Ordering::Relaxed), pages_after);
    index.search_knn(&[0.0, 0.0], 0).unwrap();
    assert_eq!(counts(&*index), Some((8, 6)));
    drop((nested, index, fixture.original));
    assert_eq!(fixture.control.memory().used(), 0);
}

#[test]
fn diskann_raw_populations_are_shared_only_by_the_same_retained_view() {
    let mut fixture = fixture();
    let (values, _) = raw(&fixture.raw, &fixture.control);
    let index = fixture
        .original
        .snapshot_with_vector_read(values, &fixture.control)
        .unwrap()
        .unwrap();
    std::thread::scope(|scope| {
        for query in [[1.0, 0.0], [0.0, 1.0]] {
            let index = &index;
            scope.spawn(move || index.search_knn(&query, 1).unwrap());
        }
    });
    assert_eq!(counts(&*index), Some((8, 6)));

    fixture.raw.delete(7).unwrap();
    let (replacement, _) = raw(&fixture.raw, &fixture.control);
    let replaced = index
        .snapshot_with_vector_read(replacement, &fixture.control)
        .unwrap()
        .unwrap();
    assert_eq!(counts(&*replaced), None);
    replaced.search_knn(&[1.0, 0.0], 1).unwrap();
    assert_eq!(counts(&*replaced), Some((6, 4)));
    assert_eq!(counts(&*index), Some((8, 6)));
    let (empty, _) = raw(&MemoryVectorIndex::new(2), &fixture.control);
    let empty = index
        .snapshot_with_vector_read(empty, &fixture.control)
        .unwrap()
        .unwrap();
    assert_eq!(counts(&*empty), None);
    assert!(empty.search_knn(&[1.0, 0.0], 1).unwrap().is_empty());
    assert_eq!(counts(&*empty), Some((0, 0)));
    fixture.control.cancellation().cancel();
    assert!(index
        .diskann_query_metadata(&StorageReadControl::with_limit(0))
        .is_err());
    assert!(replaced
        .diskann_query_metadata(&StorageReadControl::with_limit(0))
        .is_err());
}

struct RejectOnce {
    source: VectorReadSnapshot,
    reject: AtomicBool,
}

impl VectorRead for RejectOnce {
    fn check_control(&self, control: &StorageReadControl) -> StorageBackendResult<()> {
        self.source.check_control(control)
    }
    fn dimensions(&self) -> u32 {
        self.source.dimensions()
    }
    fn next_document_after(
        &self,
        after: Option<DocId>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<DocId>> {
        self.source.next_document_after(after, control)
    }
    fn document_vector_count(
        &self,
        document: DocId,
        control: &StorageReadControl,
    ) -> StorageBackendResult<u64> {
        self.source.document_vector_count(document, control)
    }
    fn read_vector(
        &self,
        document: DocId,
        ordinal: u32,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<BudgetedVec<f32>>> {
        if document == 7 && self.reject.swap(false, Ordering::SeqCst) {
            return Err(invalid("last raw document read failed"));
        }
        self.source.read_vector(document, ordinal, control)
    }
}

#[test]
fn diskann_raw_population_failure_does_not_publish_a_partial_count() {
    let fixture = fixture();
    let (source, _) = raw(&fixture.raw, &fixture.control);
    let index = fixture
        .original
        .snapshot_with_vector_read(
            Arc::new(RejectOnce {
                source,
                reject: AtomicBool::new(true),
            }),
            &fixture.control,
        )
        .unwrap()
        .unwrap();
    let error = index.search_knn(&[1.0, 0.0], 1).unwrap_err();
    assert!(error.to_string().contains("last raw document read failed"));
    assert_eq!(counts(&*index), None);
    assert_eq!(
        bits(&index.search_knn(&[1.0, 0.0], 1).unwrap()),
        [(1, 1.0_f64.to_bits())]
    );
    assert_eq!(counts(&*index), Some((8, 6)));
}
