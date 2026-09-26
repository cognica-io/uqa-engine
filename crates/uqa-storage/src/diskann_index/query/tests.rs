//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::diskann_index::{
    build::{
        DiskANNBuildCapture, DiskANNGenerationOptions, DiskANNMergeOptions,
        DiskANNPartitionOptions, DiskANNTemporaryBudget,
    },
    format::{DiskANNChangeIdentity, DiskANNGeneration, DiskANNVectorVersion, PAGE_BYTES},
    pages::{DiskANNMemoryBuilder, DiskANNMemorySource},
    DiskANNCanonicalRead, DiskANNCanonicalVectorVisitor, PQTrainingOptions,
};
use crate::mvcc::{DatabaseId, StorageTransactionId};
use crate::{MemoryVectorIndex, VectorIndex};
use std::collections::{BTreeMap, BTreeSet};
use std::ops::Bound::{Excluded, Unbounded};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Mutex,
};
use uqa_core::{memory::MemoryBudget, DocId};

mod failures;

#[derive(Clone)]
struct Source {
    documents: BTreeMap<DocId, (u64, Vec<Vec<f32>>)>,
    changes: BTreeSet<DocId>,
    control: StorageReadControl,
    visits: Arc<Mutex<BTreeMap<DocId, usize>>>,
    scans: Arc<AtomicUsize>,
}

impl Source {
    fn new(documents: impl IntoIterator<Item = (DocId, Vec<Vec<f32>>)>) -> Self {
        let documents: BTreeMap<_, _> = documents
            .into_iter()
            .map(|(doc, values)| (doc, (1, values)))
            .collect();
        Self {
            changes: documents.keys().copied().collect(),
            documents,
            control: StorageReadControl::with_limit(1 << 20),
            visits: Arc::default(),
            scans: Arc::default(),
        }
    }
    fn replace(&mut self, doc: DocId, values: Vec<Vec<f32>>) {
        self.documents.insert(doc, (2, values));
        self.changes.insert(doc);
    }
    fn reset(&self) {
        self.visits.lock().unwrap().clear();
        self.scans.store(0, Ordering::Relaxed);
    }
}

fn version(revision: u64) -> DiskANNVectorVersion {
    DiskANNVectorVersion::new(
        StorageTransactionId::new(DatabaseId::from_bytes([71; 16]), 3).unwrap(),
        revision,
    )
    .unwrap()
}

impl DiskANNCanonicalRead for Source {
    fn check_control(&self, control: &StorageReadControl) -> StorageBackendResult<()> {
        self.control.check()?;
        control.check()
    }
    fn dimensions(&self) -> u32 {
        2
    }
    fn next_document_after(
        &self,
        after: Option<DocId>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<DocId>> {
        self.check_control(control)?;
        self.scans.fetch_add(1, Ordering::Relaxed);
        Ok(self
            .documents
            .range((after.map_or(Unbounded, Excluded), Unbounded))
            .next()
            .map(|(&doc, _)| doc))
    }
    fn origin(
        &self,
        document: DocId,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<DiskANNVectorVersion>> {
        self.check_control(control)?;
        Ok(self
            .documents
            .get(&document)
            .map(|(revision, _)| version(*revision)))
    }
    fn visit_document(
        &self,
        document: DocId,
        control: &StorageReadControl,
        visit: &mut DiskANNCanonicalVectorVisitor<'_>,
    ) -> StorageBackendResult<Option<DiskANNVectorVersion>> {
        self.check_control(control)?;
        let Some((revision, values)) = self.documents.get(&document) else {
            return Ok(None);
        };
        *self.visits.lock().unwrap().entry(document).or_default() += 1;
        for (ordinal, raw) in values.iter().enumerate() {
            visit(ordinal as u32, version(*revision), raw)?;
        }
        self.check_control(control)?;
        Ok(Some(version(*revision)))
    }
}

impl DiskANNQueryRead for Source {
    fn next_change_after(
        &self,
        after: Option<DocId>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<DiskANNChangeIdentity>> {
        self.check_control(control)?;
        Ok(self
            .changes
            .range((after.map_or(Unbounded, Excluded), Unbounded))
            .next()
            .map(|&doc| DiskANNChangeIdentity::new(doc, version(self.documents[&doc].0))))
    }
}

fn parameters() -> DiskANNIndexParams {
    DiskANNIndexParams {
        max_degree: 2,
        build_list_size: 4,
        search_list_size: 1,
        beam_width: 1,
        pq_bytes: 1,
        ..DiskANNIndexParams::for_dimensions(2).unwrap()
    }
}

fn limits() -> DiskANNReadLimits {
    DiskANNReadLimits {
        resident_bytes: 65_536,
        cache_bytes: 0,
        max_in_flight_page_bytes: 2 * PAGE_BYTES,
        max_record_bytes: 8192,
    }
}

fn build(source: &Source) -> Arc<DiskANNMemorySource> {
    let directory = tempfile::tempdir().unwrap();
    let control = StorageReadControl::with_limit(1 << 20);
    let temporary = DiskANNTemporaryBudget::new(1 << 20);
    let capture = DiskANNBuildCapture::capture(
        DiskANNGeneration::new([72; 16], 1, 2, 3).unwrap(),
        source.clone(),
        directory.path(),
        &temporary,
        &control,
    )
    .unwrap();
    let training = PQTrainingOptions {
        max_samples: 8,
        max_centroids: 2,
        max_iterations: 2,
        seed: 42,
    };
    let runs = capture
        .input()
        .build_partitions(
            directory.path(),
            parameters(),
            DiskANNPartitionOptions {
                max_partition_points: 16,
                coarse_training: PQTrainingOptions {
                    max_centroids: 3,
                    ..training
                },
                max_depth: 0,
            },
        )
        .unwrap();
    let graph = capture
        .input()
        .merge_partitions(
            runs,
            directory.path(),
            DiskANNMergeOptions {
                sort_buffer_records: 16,
            },
        )
        .unwrap();
    let physical = MemoryBudget::new(1 << 20);
    let mut sink = DiskANNMemoryBuilder::new(capture.input().coverage().generation(), &physical);
    let manifest = capture
        .write_generation(
            &graph,
            DiskANNGenerationOptions {
                training,
                code_batch_nodes: 4,
                side_batch_entries: 4,
                max_record_bytes: 8192,
            },
            &mut sink,
        )
        .unwrap();
    let source = Arc::new(sink.finish(manifest, &control).unwrap());
    drop(capture.finish(&manifest, &control).unwrap());
    drop(graph);
    assert_eq!(temporary.used(), 0);
    source
}

fn ids(postings: &PostingList) -> Vec<DocId> {
    postings.doc_ids().collect()
}
fn bits(postings: &PostingList) -> Vec<(DocId, u64)> {
    postings
        .iter()
        .map(|p| (p.doc_id, p.payload.score.to_bits()))
        .collect()
}

#[test]
fn diskann_document_query_merges_uncovered_changes_and_side_without_stale_scores() {
    let base = Source::new([
        (1, vec![vec![-1.0, 0.0], vec![1.0, 0.0]]),
        (2, vec![vec![0.0, 1.0]]),
        (3, vec![vec![0.0, 0.0]]),
        (4, vec![vec![-1.0, 0.0]]),
        (5, vec![]),
        (6, vec![vec![3.0, 4.0]]),
    ]);
    let physical = build(&base);
    let mut live = base.clone();
    live.replace(1, vec![vec![0.0, 1.0]]);
    live.replace(2, vec![]);
    live.replace(5, vec![vec![1.0, 0.0]]);
    live.replace(7, vec![vec![-1.0, 0.0], vec![1.0, 0.0]]);
    let mut pruned = live.clone();
    pruned.changes.retain(|doc| live.documents[doc].0 != 1);
    let control = StorageReadControl::with_limit(1 << 20);
    let query =
        DiskANNQuery::open(&live, physical.clone(), parameters(), limits(), &control).unwrap();
    let clean = DiskANNQuery::open(&pruned, physical, parameters(), limits(), &control).unwrap();
    live.reset();
    for k in 0..=10 {
        let actual = query.search_knn(&[1.0, 0.0], k, &control).unwrap();
        assert_eq!(
            bits(&actual.postings),
            bits(&clean.search_knn(&[1.0, 0.0], k, &control).unwrap().postings)
        );
        assert_eq!(actual.postings.len(), k.min(6));
        assert!(actual.exact_reason.is_none());
    }
    assert_eq!(
        live.scans.load(Ordering::Relaxed),
        0,
        "ordinary ANN never scans the canonical corpus"
    );
    assert_eq!(
        ids(&query.search_knn(&[1.0, 0.0], 1, &control).unwrap().postings),
        [5]
    );
    assert_eq!(
        ids(&query.search_knn(&[1.0, 0.0], 2, &control).unwrap().postings),
        [5, 7]
    );
    let all = query
        .search_knn(&[1.0, 0.0], 10, &control)
        .unwrap()
        .postings;
    assert_eq!(
        bits(&all),
        [
            (1, 0.0_f32),
            (3, 0.0),
            (4, -1.0),
            (5, 1.0),
            (6, 0.6),
            (7, 1.0)
        ]
        .map(|(doc, score)| (doc, f64::from(score).to_bits()))
    );
    assert_eq!(
        ids(&query.search_threshold(&[1.0, 0.0], 0.6, &control).unwrap()),
        [5, 6, 7]
    );
}

#[test]
fn diskann_document_query_completes_after_tensor_collapse_and_scores_each_graph_document_once() {
    let base = Source::new([
        (1, vec![vec![1.0, 0.0]; 8]),
        (2, vec![vec![0.0, 1.0]]),
        (3, vec![vec![-1.0, 0.0]]),
        (4, vec![vec![-3.0, 4.0]]),
    ]);
    let physical = build(&base);
    let owner = StorageReadControl::with_limit(1 << 20);
    let query = DiskANNQuery::open(&base, physical, parameters(), limits(), &owner).unwrap();
    base.reset();
    let control = StorageReadControl::with_limit(65_536);
    let actual = query.search_knn(&[1.0, 0.0], 4, &control).unwrap();
    assert_eq!(ids(&actual.postings), [1, 2, 3, 4]);
    assert!(actual.traversal.completion_expansions > 0);
    assert!(base
        .visits
        .lock()
        .unwrap()
        .values()
        .all(|&count| count == 1));
    assert_eq!(base.scans.load(Ordering::Relaxed), 0);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn diskann_document_query_streams_large_changes_with_top_k_workspace() {
    let empty = Source::new([]);
    let physical = build(&empty);
    let live = Source::new((0..4096).map(|doc| (doc, vec![vec![1.0, 0.0], vec![0.0, 1.0]])));
    let owner = StorageReadControl::with_limit(1 << 20);
    let query = DiskANNQuery::open(&live, physical, parameters(), limits(), &owner).unwrap();
    let control = StorageReadControl::with_limit(4096);
    let actual = query.search_knn(&[1.0, 0.0], 3, &control).unwrap();
    assert_eq!(ids(&actual.postings), [0, 1, 2]);
    assert_eq!(actual.traversal, DiskANNTraversalStats::default());
    assert_eq!(live.scans.load(Ordering::Relaxed), 0);
    assert_eq!(control.memory().used(), 0);
    assert!(control.memory().peak() <= 4096);
}

#[test]
fn diskann_document_query_side_scores_outrank_negative_graph_scores_without_duplicate_documents() {
    let base = Source::new([
        (1, vec![vec![-1.0, 0.0], vec![0.0, 0.0]]),
        (2, vec![vec![-1.0, 0.0]]),
        (3, vec![vec![0.0, 0.0]]),
    ]);
    let physical = build(&base);
    let owner = StorageReadControl::with_limit(1 << 20);
    let query = DiskANNQuery::open(&base, physical, parameters(), limits(), &owner).unwrap();
    let control = StorageReadControl::with_limit(65_536);
    assert_eq!(
        ids(&query.search_knn(&[1.0, 0.0], 2, &control).unwrap().postings),
        [1, 3]
    );
    assert_eq!(
        bits(&query.search_knn(&[1.0, 0.0], 3, &control).unwrap().postings),
        [
            (1, 0.0_f64.to_bits()),
            (2, (-1.0_f64).to_bits()),
            (3, 0.0_f64.to_bits())
        ]
    );
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn diskann_document_query_preserves_numeric_scores_thresholds_and_exact_route_reasons() {
    let base = Source::new([
        (1, vec![vec![0.0, -0.0]]),
        (2, vec![vec![f32::from_bits(1), 0.0]]),
        (3, vec![vec![f32::MAX, f32::MAX], vec![1.0, 0.0]]),
        (4, vec![vec![-f32::MAX, -f32::MAX]]),
        (5, vec![vec![-1.0, 0.0]]),
    ]);
    let physical = build(&base);
    let control = StorageReadControl::with_limit(1 << 20);
    let query = DiskANNQuery::open(&base, physical, parameters(), limits(), &control).unwrap();
    let mut exact = MemoryVectorIndex::new(2);
    for (&doc, (_, values)) in &base.documents {
        exact.add_many(doc, values.clone()).unwrap();
    }
    for vector in [[0.0, 0.0], [f32::from_bits(1), 0.0], [f32::MAX, f32::MAX]] {
        for k in 1..=6 {
            let actual = query.search_knn(&vector, k, &control).unwrap();
            assert!(actual.exact_reason.is_some());
            assert_eq!(actual.traversal, DiskANNTraversalStats::default());
            assert_eq!(
                bits(&actual.postings),
                bits(&exact.search_knn(&vector, k).unwrap())
            );
        }
    }
    for vector in [[1.0, 0.0], [f32::MAX, f32::MAX]] {
        for threshold in [-1.0, 0.0, 0.6, 1.0] {
            assert_eq!(
                bits(
                    &query
                        .search_threshold(&vector, threshold, &control)
                        .unwrap()
                ),
                bits(&exact.search_threshold(&vector, threshold).unwrap())
            );
        }
    }
    assert_eq!(
        bits(
            &query
                .search_knn(&[1.0, 0.0], 10, &control)
                .unwrap()
                .postings
        ),
        bits(&exact.search_knn(&[1.0, 0.0], 10).unwrap())
    );
}
