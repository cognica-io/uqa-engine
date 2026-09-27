//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::diskann_index::{
    format::DiskANNManifest,
    pages::{DiskANNPageVisitor, DiskANNReadCapabilities, DiskANNRecordKey, DiskANNRecordVisitor},
    DiskANNCanonicalCounts, DiskANNQueryMetadata,
};

struct MetadataOnly {
    source: Arc<DiskANNMemorySource>,
    reads: AtomicUsize,
    reject: bool,
}

struct CountedCanonical {
    generation: DiskANNGeneration,
    control: StorageReadControl,
}

impl DiskANNCanonicalRead for CountedCanonical {
    fn population_counts(
        &self,
        generation: DiskANNGeneration,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<DiskANNCanonicalCounts>> {
        self.check_control(control)?;
        assert_eq!(generation, self.generation);
        Ok(Some(DiskANNCanonicalCounts::new(5, 4)?))
    }
    fn check_control(&self, control: &StorageReadControl) -> StorageBackendResult<()> {
        self.control.check()?;
        control.check()
    }
    fn dimensions(&self) -> u32 {
        2
    }
    fn next_document_after(
        &self,
        _: Option<DocId>,
        _: &StorageReadControl,
    ) -> StorageBackendResult<Option<DocId>> {
        panic!("population capture must not enumerate canonical documents")
    }
    fn origin(
        &self,
        _: DocId,
        _: &StorageReadControl,
    ) -> StorageBackendResult<Option<DiskANNVectorVersion>> {
        panic!("population capture must not reconstruct origin coverage")
    }
    fn visit_document(
        &self,
        _: DocId,
        _: &StorageReadControl,
        _: &mut DiskANNCanonicalVectorVisitor<'_>,
    ) -> StorageBackendResult<Option<DiskANNVectorVersion>> {
        panic!("population capture must not read coordinates")
    }
}

#[test]
fn diskann_population_capture_uses_bounded_owner_metadata_without_corpus_reads() {
    let original = Source::new([(1, vec![vec![1.0, 0.0]]), (2, vec![vec![0.0, 0.0]])]);
    let physical = MetadataOnly {
        source: build(&original),
        reads: AtomicUsize::new(0),
        reject: false,
    };
    let canonical = CountedCanonical {
        generation: physical.generation(),
        control: StorageReadControl::with_limit(4096),
    };
    let control = StorageReadControl::with_limit(4096);
    let metadata =
        DiskANNQueryMetadata::capture(&canonical, &physical, parameters(), limits(), &control)
            .unwrap();
    let statistics = metadata
        .index_statistics(&StorageReadControl::with_limit(0))
        .unwrap();
    assert_eq!(statistics.populations.base_vectors, 2);
    assert_eq!(statistics.populations.current_vectors, Some(5));
    assert_eq!(statistics.populations.changed_vectors, Some(4));
    assert_eq!(physical.reads.load(Ordering::Relaxed), 1);
    assert_eq!(control.memory().used(), 0);
    canonical.control.cancellation().cancel();
    assert!(
        DiskANNQueryMetadata::capture(&canonical, &physical, parameters(), limits(), &control)
            .is_err()
    );
    assert_eq!(physical.reads.load(Ordering::Relaxed), 1);
}

impl DiskANNPageSource for MetadataOnly {
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
        assert_eq!(
            key,
            DiskANNRecordKey::Manifest,
            "metadata must not prepare PQ or origins"
        );
        assert!(limit <= DiskANNManifest::MAX_ENCODED_BYTES);
        self.reads.fetch_add(1, Ordering::Relaxed);
        if self.reject {
            return Err(invalid("metadata source rejected the read"));
        }
        self.source.read_record(key, limit, control, visit)
    }
    fn read_graph_pages(
        &self,
        _: &[u64],
        _: &StorageReadControl,
        _: &mut DiskANNPageVisitor<'_>,
    ) -> StorageBackendResult<()> {
        panic!("metadata must not read graph pages")
    }
}

#[test]
fn diskann_metadata_needs_only_one_bounded_manifest_and_no_search_workspace() {
    let canonical = Source::new([(1, vec![vec![1.0, 0.0]]), (2, vec![vec![0.0, 0.0]])]);
    let physical = MetadataOnly {
        source: build(&canonical),
        reads: AtomicUsize::new(0),
        reject: false,
    };
    canonical.reset();
    let control = StorageReadControl::with_limit(4096);
    let mut read_limits = limits();
    read_limits.resident_bytes = 0;
    read_limits.cache_bytes = 0;
    read_limits.max_in_flight_page_bytes = 0;
    let metadata =
        DiskANNQueryMetadata::capture(&canonical, &physical, parameters(), read_limits, &control)
            .unwrap();
    assert_eq!(physical.reads.load(Ordering::Relaxed), 1);
    assert_eq!(metadata.manifest.input().generation, physical.generation());
    assert_eq!(metadata.manifest.input().nodes, 1);
    assert_eq!(metadata.manifest.input().side_vectors, 1);
    assert_eq!(metadata.manifest.input().coverage.vector_count(), 2);
    assert_eq!(metadata.read_limits, read_limits);
    assert_eq!(metadata.read_capabilities, physical.capabilities());
    assert_eq!(canonical.scans.load(Ordering::Relaxed), 0);
    assert!(canonical.visits.lock().unwrap().is_empty());
    assert_eq!(control.memory().used(), 0);

    let exhausted = StorageReadControl::with_limit(1);
    assert!(DiskANNQueryMetadata::capture(
        &canonical,
        &physical,
        parameters(),
        read_limits,
        &exhausted,
    )
    .is_err());
    assert_eq!(exhausted.memory().used(), 0);
    canonical.control.cancellation().cancel();
    let before = physical.reads.load(Ordering::Relaxed);
    assert!(DiskANNQueryMetadata::capture(
        &canonical,
        &physical,
        parameters(),
        read_limits,
        &control,
    )
    .is_err());
    assert_eq!(physical.reads.load(Ordering::Relaxed), before);
}

#[test]
fn diskann_metadata_propagates_definition_source_and_invocation_failures() {
    let canonical = Source::new([(1, vec![vec![1.0, 0.0]])]);
    let mut physical = MetadataOnly {
        source: build(&canonical),
        reads: AtomicUsize::new(0),
        reject: false,
    };
    canonical.reset();
    let control = StorageReadControl::with_limit(4096);
    let mut wrong = parameters();
    wrong.search_list_size += 1;
    assert!(
        DiskANNQueryMetadata::capture(&canonical, &physical, wrong, limits(), &control).is_err()
    );
    physical.reject = true;
    assert!(
        DiskANNQueryMetadata::capture(&canonical, &physical, parameters(), limits(), &control,)
            .is_err()
    );
    control.cancellation().cancel();
    let before = physical.reads.load(Ordering::Relaxed);
    assert!(
        DiskANNQueryMetadata::capture(&canonical, &physical, parameters(), limits(), &control,)
            .is_err()
    );
    assert_eq!(physical.reads.load(Ordering::Relaxed), before);
    assert_eq!(canonical.scans.load(Ordering::Relaxed), 0);
    assert!(canonical.visits.lock().unwrap().is_empty());
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn diskann_physical_statistics_classify_queries_without_artifact_or_corpus_reads() {
    use uqa_core::VectorQueryRoute::{Approximate, ExactNonFiniteNorm, ExactZeroNorm};
    let canonical = Source::new([(1, vec![vec![1.0, 0.0]]), (2, vec![vec![0.0, 0.0]])]);
    let physical = MetadataOnly {
        source: build(&canonical),
        reads: AtomicUsize::new(0),
        reject: false,
    };
    canonical.reset();
    let capture = StorageReadControl::with_limit(4096);
    let metadata =
        DiskANNQueryMetadata::capture(&canonical, &physical, parameters(), limits(), &capture)
            .unwrap();
    let control = StorageReadControl::with_limit(0);
    let field = metadata.index_statistics(&control).unwrap();
    for (query, route) in [
        ([1.0, 0.0], Approximate),
        ([0.0, 0.0], ExactZeroNorm),
        ([f32::from_bits(1), 0.0], ExactZeroNorm),
        ([f32::MAX, 0.0], ExactNonFiniteNorm),
    ] {
        let stats = metadata.query_statistics(&query, &control).unwrap();
        assert_eq!(stats.index, field);
        assert_eq!(stats.query_route, route);
        assert_eq!(stats.index.dimensions, 2);
        assert_eq!(stats.index.populations.base_documents, Some(2));
        assert_eq!(
            (
                stats.index.populations.graph_nodes,
                stats.index.populations.side_vectors,
                stats.index.populations.base_vectors
            ),
            (1, 1, 2)
        );
        assert_eq!(stats.index.populations.changed_vectors, None);
        assert_eq!(stats.index.populations.current_vectors, None);
        assert_eq!(stats.index.pq_centroids, Some(1));
        assert_eq!(stats.index.node_fragments, 1);
    }
    assert!(metadata
        .query_statistics(&[f32::NAN, 0.0], &control)
        .is_err());
    assert!(metadata.query_statistics(&[1.0], &control).is_err());
    control.cancellation().cancel();
    assert!(metadata.index_statistics(&control).is_err());
    assert!(metadata.query_statistics(&[1.0, 0.0], &control).is_err());
    assert_eq!(physical.reads.load(Ordering::Relaxed), 1);
    assert_eq!(canonical.scans.load(Ordering::Relaxed), 0);
    assert!(canonical.visits.lock().unwrap().is_empty());
    assert_eq!(control.memory().used(), 0);
}
