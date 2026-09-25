//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::diskann_index::DiskANNCanonicalVectorVisitor;
use crate::mvcc::{DatabaseId, StorageTransactionId};
use crate::{MemoryVectorIndex, VectorIndex};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::ops::Bound::{Excluded, Unbounded};

mod failures;

fn version(revision: u64) -> DiskANNVectorVersion {
    DiskANNVectorVersion::new(
        StorageTransactionId::new(DatabaseId::from_bytes([17; 16]), 3).unwrap(),
        revision,
    )
    .unwrap()
}

#[derive(Deserialize)]
struct Fixture {
    scores: Scores,
}
#[derive(Deserialize)]
struct Scores {
    query: Vec<f32>,
    documents: Vec<Document>,
    ranked_doc_ids: Vec<DocId>,
    posting_doc_ids: Vec<DocId>,
}
#[derive(Deserialize)]
struct Document {
    id: DocId,
    vectors: Vec<Vec<f32>>,
    score_bits: Option<String>,
}

struct Source {
    documents: BTreeMap<DocId, Vec<Vec<f32>>>,
    control: StorageReadControl,
}

impl Source {
    fn new(documents: impl IntoIterator<Item = (DocId, Vec<Vec<f32>>)>) -> Self {
        Self {
            documents: documents.into_iter().collect(),
            control: StorageReadControl::with_limit(1 << 20),
        }
    }
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
        Ok(self
            .documents
            .range((after.map_or(Unbounded, Excluded), Unbounded))
            .next()
            .map(|(&id, _)| id))
    }
    fn origin(
        &self,
        document: DocId,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<DiskANNVectorVersion>> {
        self.check_control(control)?;
        Ok(self.documents.get(&document).map(|_| version(1)))
    }
    fn visit_document(
        &self,
        document: DocId,
        control: &StorageReadControl,
        visit: &mut DiskANNCanonicalVectorVisitor<'_>,
    ) -> StorageBackendResult<Option<DiskANNVectorVersion>> {
        self.check_control(control)?;
        let Some(vectors) = self.documents.get(&document) else {
            return Ok(None);
        };
        for (ordinal, raw) in vectors.iter().enumerate() {
            self.check_control(control)?;
            visit(ordinal as u32, version(1), raw)?;
        }
        self.check_control(control)?;
        Ok(Some(version(1)))
    }
}

fn ids(postings: &PostingList) -> Vec<DocId> {
    postings.doc_ids().collect()
}

#[test]
fn diskann_canonical_scoring_matches_the_independent_tensor_bits_and_document_ties() {
    let fixture: Fixture = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/diskann/reference.json"
    )))
    .unwrap();
    let expected = fixture.scores;
    let source = Source::new(
        expected
            .documents
            .iter()
            .map(|doc| (doc.id, doc.vectors.clone())),
    );
    let control = StorageReadControl::with_limit(8192);
    let scorer = DiskANNCanonicalScorer::new(&source, &expected.query, &control).unwrap();
    for document in &expected.documents {
        let score = scorer.score_document(document.id).unwrap();
        assert_eq!(
            score.map(|score| score.raw_cosine().to_bits()),
            document
                .score_bits
                .as_ref()
                .map(|bits| u32::from_str_radix(bits, 16).unwrap())
        );
        if let Some(score) = score {
            assert_eq!(score.document(), document.id);
            assert_eq!(score.vector_count(), document.vectors.len() as u64);
            assert_eq!(score.version(), version(1));
        }
    }
    assert!(scorer.score_document(99).unwrap().is_none());
    let full = scorer.search_exact_knn(usize::MAX).unwrap();
    assert_eq!(ids(&full), expected.posting_doc_ids);
    for k in 0..=expected.documents.len() {
        let mut wanted = expected.ranked_doc_ids[..k.min(expected.ranked_doc_ids.len())].to_vec();
        wanted.sort_unstable();
        assert_eq!(ids(&scorer.search_exact_knn(k).unwrap()), wanted);
    }
    for threshold in [-2.0, -1.0, 0.0, 0.6, 1.0, 2.0] {
        let wanted: Vec<_> = expected
            .documents
            .iter()
            .filter_map(|doc| {
                let bits = u32::from_str_radix(doc.score_bits.as_ref()?, 16).unwrap();
                (f32::from_bits(bits) >= threshold).then_some(doc.id)
            })
            .collect();
        assert_eq!(ids(&scorer.search_threshold(threshold).unwrap()), wanted);
    }
    let score = scorer.score_candidate(2, 0, version(1)).unwrap().unwrap();
    assert_eq!(score.raw_cosine().to_bits(), 1.0_f32.to_bits());
    assert_eq!(score.vector_count(), 2);
    assert!(scorer.score_candidate(2, 0, version(2)).unwrap().is_none());
    assert!(scorer.score_candidate(99, 0, version(1)).unwrap().is_none());
    assert!(scorer.score_candidate(2, 2, version(1)).is_err());
    assert!(scorer.score_candidate(5, 0, version(1)).is_err());
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn diskann_canonical_scoring_preserves_numeric_edge_and_threshold_observations() {
    let source = Source::new([
        (1, vec![vec![0.0, -0.0]]),
        (2, vec![vec![f32::from_bits(1), 0.0]]),
        (3, vec![vec![f32::MAX, f32::MAX], vec![1.0, 0.0]]),
        (4, vec![vec![-f32::MAX, -f32::MAX]]),
        (DocId::MAX, vec![vec![f32::MAX, f32::MAX]]),
    ]);
    let mut reference = MemoryVectorIndex::new(2);
    for (&doc, values) in &source.documents {
        reference.add_many(doc, values.clone()).unwrap();
    }
    let control = StorageReadControl::with_limit(8192);
    for query in [
        [1.0, 0.0],
        [0.0, 0.0],
        [f32::from_bits(1), 0.0],
        [f32::MAX, f32::MAX],
    ] {
        let scorer = DiskANNCanonicalScorer::new(&source, &query, &control).unwrap();
        for k in 0..=7 {
            let expected = reference.search_knn(&query, k).unwrap();
            let actual = scorer.search_exact_knn(k).unwrap();
            assert_eq!(ids(&actual), ids(&expected));
            assert!(actual
                .iter()
                .zip(expected.iter())
                .all(|(a, b)| a.payload.score.to_bits() == b.payload.score.to_bits()));
        }
        for threshold in [-1.0, 0.0, 1.0] {
            assert_eq!(
                ids(&scorer.search_threshold(threshold).unwrap()),
                ids(&reference.search_threshold(&query, threshold).unwrap())
            );
        }
    }
    assert!(
        DiskANNCanonicalScorer::new(&source, &[f32::MAX, f32::MAX], &control)
            .unwrap()
            .score_document(DocId::MAX)
            .unwrap()
            .unwrap()
            .raw_cosine()
            .is_nan()
    );
}

#[test]
fn diskann_exact_top_k_workspace_does_not_grow_with_the_corpus() {
    let source = Source::new((0..4096).map(|doc| (doc, vec![vec![1.0, 0.0], vec![0.0, 1.0]])));
    let control = StorageReadControl::with_limit(2048);
    let scorer = DiskANNCanonicalScorer::new(&source, &[1.0, 0.0], &control).unwrap();
    let selected = scorer.search_exact_knn(3).unwrap();
    assert_eq!(ids(&selected), [0, 1, 2]);
    assert!(control.memory().peak() <= 2048);
    assert_eq!(control.memory().used(), 0);
    assert!(scorer.search_threshold(0.0).is_err());
    assert_eq!(control.memory().used(), 0);
    let empty = Source::new([]);
    let scorer = DiskANNCanonicalScorer::new(&empty, &[1.0, 0.0], &control).unwrap();
    assert!(scorer.search_exact_knn(usize::MAX).unwrap().is_empty());
}
