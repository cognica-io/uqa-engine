//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::diskann_index::{
    format::DiskANNCanonicalOrigin, DiskANNCanonicalCounts, DiskANNReadChanges,
};

struct PopulationSource {
    source: Source,
    counts: Option<DiskANNCanonicalCounts>,
    points: Arc<AtomicUsize>,
    selected: Option<DocId>,
}

impl DiskANNCanonicalRead for PopulationSource {
    fn population_counts(
        &self,
        _: DiskANNGeneration,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<DiskANNCanonicalCounts>> {
        self.check_control(control)?;
        Ok(self.counts)
    }
    fn check_control(&self, control: &StorageReadControl) -> StorageBackendResult<()> {
        self.source.check_control(control)
    }
    fn dimensions(&self) -> u32 {
        2
    }
    fn next_document_after(
        &self,
        _: Option<DocId>,
        _: &StorageReadControl,
    ) -> StorageBackendResult<Option<DocId>> {
        panic!("selection population must not enumerate the corpus")
    }
    fn origin(
        &self,
        _: DocId,
        _: &StorageReadControl,
    ) -> StorageBackendResult<Option<DiskANNVectorVersion>> {
        panic!("selection population needs complete point metadata")
    }
    fn visit_document(
        &self,
        _: DocId,
        _: &StorageReadControl,
        _: &mut DiskANNCanonicalVectorVisitor<'_>,
    ) -> StorageBackendResult<Option<DiskANNVectorVersion>> {
        panic!("selection population must not read coordinates")
    }
}

impl DiskANNQueryRead for PopulationSource {
    fn document_origin(
        &self,
        document: DocId,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<DiskANNCanonicalOrigin>> {
        assert_eq!(
            Some(document),
            self.selected,
            "only selected points may be read"
        );
        self.points.fetch_add(1, Ordering::Relaxed);
        self.source.document_origin(document, control)
    }
    fn next_change_after(
        &self,
        _: Option<DocId>,
        _: &StorageReadControl,
    ) -> StorageBackendResult<Option<DiskANNChangeIdentity>> {
        panic!("selection population must not enumerate the journal")
    }
}

#[test]
fn diskann_selected_populations_use_only_selected_origins_then_retain_fixed_metadata() {
    for complete in [false, true] {
        let source = Source::new([
            (1, vec![vec![1.0, 0.0], vec![0.0, 1.0]]),
            (2, vec![vec![0.0, 0.0]]),
        ]);
        let physical = Observed::new(&source);
        let control = StorageReadControl::with_limit(65_536);
        let points = Arc::new(AtomicUsize::new(0));
        let base = PopulationSource {
            source: source.clone(),
            counts: (!complete).then(|| DiskANNCanonicalCounts::new(3, 0).unwrap()),
            points: points.clone(),
            selected: (!complete).then_some(1),
        };
        let index =
            RetainedDiskANNIndex::open(base, physical.clone(), parameters(), limits(), &control)
                .unwrap();
        let mut private = source;
        private.control = StorageReadControl::with_limit(1 << 20);
        private.replace(1, vec![vec![-1.0, 0.0]]);
        let cancel = private.control.cancellation().clone();
        let newer = index
            .with_canonical(PopulationSource {
                source: private,
                counts: None,
                points: points.clone(),
                selected: Some(1),
            })
            .unwrap();
        let changes = DiskANNReadChanges::capture(
            [Ok((1, newer.diskann_read_snapshot(&control).unwrap()))],
            &control,
        )
        .unwrap();
        let changes = if complete {
            changes.without_unselected_documents()
        } else {
            changes
        };
        let before = physical.records.load(Ordering::Relaxed);
        let selected = index
            .snapshot_with_diskann_changes(&changes, &control)
            .unwrap()
            .unwrap();
        let expected = DiskANNCanonicalCounts::new(if complete { 1 } else { 2 }, 1).unwrap();
        assert_eq!(points.load(Ordering::Relaxed), if complete { 1 } else { 2 });
        assert_eq!(physical.records.load(Ordering::Relaxed), before + 1);
        for _ in 0..2 {
            let metadata = selected
                .diskann_query_metadata(&StorageReadControl::with_limit(0))
                .unwrap()
                .unwrap();
            assert_eq!(metadata.canonical_counts, Some(expected));
        }
        assert_eq!(points.load(Ordering::Relaxed), if complete { 1 } else { 2 });
        assert_eq!(physical.records.load(Ordering::Relaxed), before + 1);
        assert_eq!(physical.pages.load(Ordering::Relaxed), 0);
        cancel.cancel();
        assert!(selected.diskann_query_metadata(&control).is_err());
        assert!(index.diskann_query_metadata(&control).is_ok());
    }
}

#[test]
fn diskann_selected_population_rejects_reused_origins_with_different_tensor_shapes() {
    let source = Source::new([(1, vec![vec![1.0, 0.0], vec![0.0, 1.0]])]);
    let physical = Observed::new(&source);
    let control = StorageReadControl::with_limit(65_536);
    let index = RetainedDiskANNIndex::open(
        PopulationSource {
            source: source.clone(),
            counts: Some(DiskANNCanonicalCounts::new(2, 0).unwrap()),
            points: Arc::default(),
            selected: Some(1),
        },
        physical,
        parameters(),
        limits(),
        &control,
    )
    .unwrap();
    let mut corrupt = source;
    corrupt.documents.get_mut(&1).unwrap().1.pop();
    let newer = index
        .with_canonical(PopulationSource {
            source: corrupt,
            counts: None,
            points: Arc::default(),
            selected: Some(1),
        })
        .unwrap();
    let changes = DiskANNReadChanges::capture(
        [Ok((1, newer.diskann_read_snapshot(&control).unwrap()))],
        &control,
    )
    .unwrap();
    let used = control.memory().used();
    assert!(index
        .snapshot_with_diskann_changes(&changes, &control)
        .is_err());
    assert_eq!(control.memory().used(), used);
    assert_eq!(
        index
            .diskann_query_metadata(&control)
            .unwrap()
            .unwrap()
            .canonical_counts,
        Some(DiskANNCanonicalCounts::new(2, 0).unwrap())
    );
}
