//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::diskann_index::pages::{
    DiskANNPageVisitor, DiskANNReadCapabilities, DiskANNRecordKey, DiskANNRecordVisitor,
};
use crate::read_control::CancellationToken;
use crate::vector_index::VectorIndexes;

struct Observed {
    source: Arc<DiskANNMemorySource>,
    records: AtomicUsize,
    pages: AtomicUsize,
    cancel: Mutex<Option<CancellationToken>>,
}

impl Observed {
    fn new(source: &Source) -> Arc<Self> {
        Arc::new(Self {
            source: build(source),
            records: AtomicUsize::new(0),
            pages: AtomicUsize::new(0),
            cancel: Mutex::new(None),
        })
    }
}

impl DiskANNPageSource for Observed {
    fn generation(&self) -> DiskANNGeneration {
        self.source.generation()
    }
    fn capabilities(&self) -> DiskANNReadCapabilities {
        self.source.capabilities()
    }
    fn read_record(
        &self,
        key: DiskANNRecordKey,
        limit: usize,
        control: &StorageReadControl,
        visit: &mut DiskANNRecordVisitor<'_>,
    ) -> StorageBackendResult<()> {
        self.records.fetch_add(1, Ordering::Relaxed);
        self.source.read_record(key, limit, control, visit)
    }
    fn read_graph_pages(
        &self,
        pages: &[u64],
        control: &StorageReadControl,
        visit: &mut DiskANNPageVisitor<'_>,
    ) -> StorageBackendResult<()> {
        self.pages.fetch_add(pages.len(), Ordering::Relaxed);
        if let Some(cancel) = self.cancel.lock().unwrap().take() {
            cancel.cancel();
        }
        self.source.read_graph_pages(pages, control, visit)
    }
}

#[test]
fn diskann_retained_metadata_counts_ordinals_without_reading_coordinates_or_pages() {
    let mut source = Source::new([
        (1, vec![vec![1.0, 0.0], vec![0.0, 1.0]]),
        (2, vec![vec![-1.0, 0.0]]),
        (3, vec![]),
    ]);
    let physical = Observed::new(&source);
    source.replace(2, vec![]);
    source.replace(4, vec![vec![0.0, 0.0]]);
    source.reset();
    let visits = source.visits.clone();
    let control = StorageReadControl::with_limit(65_536);
    let index =
        RetainedDiskANNIndex::open(source, physical.clone(), parameters(), limits(), &control)
            .unwrap();
    let records = physical.records.load(Ordering::Relaxed);
    assert_eq!(index.dimensions(), 2);
    assert_eq!(index.index_kind(), "diskann");
    assert_eq!(
        index.manifest().input().coverage.generation(),
        physical.generation()
    );
    assert_eq!(index.count().unwrap(), 3);
    for (document, present) in [(1, true), (2, false), (3, false), (4, true), (99, false)] {
        assert_eq!(index.contains_document(document).unwrap(), present);
    }
    assert_eq!(physical.records.load(Ordering::Relaxed), records);
    assert_eq!(physical.pages.load(Ordering::Relaxed), 0);
    assert!(visits.lock().unwrap().is_empty());
    assert_eq!(ids(&index.search_knn(&[1.0, 0.0], 99).unwrap()), [1, 4]);
}

#[test]
fn diskann_nested_snapshots_share_preparation_and_release_the_last_owner() {
    let source = Source::new([(1, vec![vec![1.0, 0.0]]), (2, vec![vec![0.0, 1.0]])]);
    let canonical_owner = Arc::downgrade(&source.visits);
    let physical = Observed::new(&source);
    let physical_owner = Arc::downgrade(&physical);
    let control = StorageReadControl::with_limit(65_536);
    let index =
        RetainedDiskANNIndex::open(source, physical.clone(), parameters(), limits(), &control)
            .unwrap();
    let used = control.memory().used();
    let records = physical.records.load(Ordering::Relaxed);
    let fresh = StorageReadControl::with_limit(1);
    let first = index.snapshot_with_control(&fresh).unwrap();
    let nested = first.snapshot_with_control(&fresh).unwrap();
    assert_eq!(control.memory().used(), used);
    assert_eq!(fresh.memory().used(), 0);
    assert_eq!(physical.records.load(Ordering::Relaxed), records);
    let mut registrations: BTreeMap<String, Box<dyn VectorIndex>> = BTreeMap::new();
    registrations.insert("vector".into(), Box::new(index));
    let collection = VectorIndexes::capture(&registrations, &control).unwrap();
    let charged = control.memory().used();
    let copied = VectorIndexes::capture(&collection, &fresh).unwrap();
    assert_eq!(control.memory().used(), charged);
    assert_eq!(fresh.memory().used(), 0);
    assert_eq!(copied.get("vector").unwrap().index_kind(), "diskann");
    drop((registrations, first, nested, collection, physical));
    assert!(canonical_owner.upgrade().is_some());
    assert!(physical_owner.upgrade().is_some());
    assert_eq!(
        ids(&copied
            .get("vector")
            .unwrap()
            .search_knn(&[1.0, 0.0], 2)
            .unwrap()),
        [1, 2]
    );
    drop(copied);
    assert_eq!(control.memory().used(), 0);
    assert!(canonical_owner.upgrade().is_none());
    assert!(physical_owner.upgrade().is_none());
}

#[test]
fn diskann_retained_queries_keep_the_original_allowance_and_both_cancellation_signals() {
    let source = Source::new([(1, vec![vec![1.0, 0.0]])]);
    let physical = Observed::new(&source);
    let owner = StorageReadControl::with_limit(65_536);
    let index =
        RetainedDiskANNIndex::open(source, physical.clone(), parameters(), limits(), &owner)
            .unwrap();
    let used = owner.memory().used();
    let hold = owner.memory().reserve(65_536 - used).unwrap();
    let fresh = StorageReadControl::with_limit(1 << 20);
    let nested = index.snapshot_with_control(&fresh).unwrap();
    assert!(nested
        .search_knn_with_control(&[1.0, 0.0], 1, &fresh)
        .is_err());
    assert!(nested
        .search_threshold_with_control(&[1.0, 0.0], 0.0, &fresh)
        .is_err());
    assert_eq!(fresh.memory().used(), 0);
    assert_eq!(owner.memory().used(), 65_536);
    assert!(nested
        .search_knn_with_control(&[1.0, 0.0], 0, &fresh)
        .unwrap()
        .is_empty());
    assert!(nested.search_knn_with_control(&[1.0], 0, &fresh).is_err());
    drop(hold);
    assert_eq!(
        ids(&nested
            .search_knn_with_control(&[1.0, 0.0], 1, &fresh)
            .unwrap()),
        [1]
    );
    *physical.cancel.lock().unwrap() = Some(fresh.cancellation().clone());
    assert!(nested
        .search_knn_with_control(&[1.0, 0.0], 1, &fresh)
        .is_err());
    assert_eq!(owner.memory().used(), used);
    assert!(nested.snapshot_with_control(&fresh).is_err());
    assert!(nested
        .search_knn_with_control(&[1.0, 0.0], 0, &fresh)
        .is_err());
    assert_eq!(ids(&nested.search_knn(&[1.0, 0.0], 1).unwrap()), [1]);
    owner.cancellation().cancel();
    assert!(nested.count().is_err());
    assert!(nested.contains_document(1).is_err());
    assert!(nested.snapshot().is_err());
    assert!(nested.search_knn(&[1.0, 0.0], 0).is_err());
    assert!(nested.search_threshold(&[1.0, 0.0], 0.0).is_err());
    drop((nested, index));
    assert_eq!(owner.memory().used(), 0);
}

#[test]
fn diskann_retained_views_reject_mutation_and_failed_open_releases_reservations() {
    let source = Source::new([(1, vec![vec![1.0, 0.0]])]);
    let physical = build(&source);
    let tiny = StorageReadControl::with_limit(1);
    assert!(RetainedDiskANNIndex::open(
        source.clone(),
        physical.clone(),
        parameters(),
        limits(),
        &tiny
    )
    .is_err());
    assert_eq!(tiny.memory().used(), 0);
    let owner = StorageReadControl::with_limit(65_536);
    let mut index =
        RetainedDiskANNIndex::open(source, physical, parameters(), limits(), &owner).unwrap();
    assert!(index.add(1, vec![0.0, 1.0]).is_err());
    assert!(index.add_many(1, vec![]).is_err());
    assert!(index.delete(1).is_err());
    assert!(index.clear().is_err());
    assert!(index.initialize().is_err());
    assert!(index.writable_snapshot().is_err());
    assert_eq!(index.count().unwrap(), 1);
    assert_eq!(
        index.search_knn(&[1.0, 0.0], 1).unwrap().entries()[0]
            .payload
            .score,
        1.0
    );
}
