//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Vector index abstraction and an in-memory brute-force implementation.
//!
//! Operators (`KNNOperator`, `VectorSimilarityOperator`,
//! `CalibratedVectorOperator`) depend only on this trait. IVF and HNSW
//! backends slot in by implementing the same surface.

use std::collections::BTreeMap;

use uqa_core::{DocId, Payload, PostingEntry, PostingList};

/// Cosine similarity between two equal-length vectors. Returns `0.0` when
/// either vector has zero norm or the dimensions differ.
///
/// Arithmetic stays in `f32` end-to-end so the result is bit-equal to
/// the reference `NumPy` implementation (`np.dot(q, v) / (||q|| * ||v||)`
/// over `float32` arrays).
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0f32;
    let mut norm_a = 0.0f32;
    let mut norm_b = 0.0f32;
    for (x, y) in a.iter().zip(b.iter()) {
        dot += x * y;
        norm_a += x * x;
        norm_b += y * y;
    }
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    dot / (norm_a.sqrt() * norm_b.sqrt())
}

pub trait VectorIndex: Send + Sync {
    fn dimensions(&self) -> u32;
    fn add(&mut self, doc_id: DocId, vector: Vec<f32>);
    fn delete(&mut self, doc_id: DocId);
    fn clear(&mut self);
    fn search_knn(&self, query: &[f32], k: usize) -> PostingList;
    fn search_threshold(&self, query: &[f32], threshold: f32) -> PostingList;
    fn count(&self) -> usize;
}

#[derive(Debug, Clone)]
pub struct MemoryVectorIndex {
    dimensions: u32,
    vectors: BTreeMap<DocId, Vec<f32>>,
}

impl MemoryVectorIndex {
    pub fn new(dimensions: u32) -> Self {
        Self {
            dimensions,
            vectors: BTreeMap::new(),
        }
    }

    pub fn vectors(&self) -> &BTreeMap<DocId, Vec<f32>> {
        &self.vectors
    }
}

impl VectorIndex for MemoryVectorIndex {
    fn dimensions(&self) -> u32 {
        self.dimensions
    }

    fn add(&mut self, doc_id: DocId, vector: Vec<f32>) {
        debug_assert_eq!(
            vector.len() as u32,
            self.dimensions,
            "vector dimension mismatch"
        );
        self.vectors.insert(doc_id, vector);
    }

    fn delete(&mut self, doc_id: DocId) {
        self.vectors.remove(&doc_id);
    }

    fn clear(&mut self) {
        self.vectors.clear();
    }

    /// Brute-force top-k by cosine similarity, descending.
    fn search_knn(&self, query: &[f32], k: usize) -> PostingList {
        if k == 0 || self.vectors.is_empty() {
            return PostingList::new();
        }
        let mut scored: Vec<(DocId, f32)> = self
            .vectors
            .iter()
            .map(|(&doc_id, v)| (doc_id, cosine_similarity(query, v)))
            .collect();
        scored.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });
        scored.truncate(k);
        // The output of `top_k` is re-sorted by doc_id ascending so the
        // posting list invariant holds; the score lives in the payload.
        scored.sort_by_key(|(id, _)| *id);
        let entries = scored
            .into_iter()
            .map(|(doc_id, sim)| PostingEntry::new(doc_id, Payload::with_score(f64::from(sim))))
            .collect::<Vec<_>>();
        PostingList::from_sorted_unchecked(entries)
    }

    /// Brute-force threshold scan: keep all docs with `cosine >= threshold`.
    fn search_threshold(&self, query: &[f32], threshold: f32) -> PostingList {
        let mut entries: Vec<PostingEntry> = self
            .vectors
            .iter()
            .filter_map(|(&doc_id, v)| {
                let sim = cosine_similarity(query, v);
                if sim >= threshold {
                    Some(PostingEntry::new(
                        doc_id,
                        Payload::with_score(f64::from(sim)),
                    ))
                } else {
                    None
                }
            })
            .collect();
        // The BTreeMap iteration is already doc_id-ascending, so the
        // filter preserves the invariant.
        entries.sort_by_key(|e| e.doc_id);
        PostingList::from_sorted_unchecked(entries)
    }

    fn count(&self) -> usize {
        self.vectors.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx_eq(a: f32, b: f32, eps: f32) {
        assert!((a - b).abs() < eps, "expected {a} ~ {b} within {eps}");
    }

    #[test]
    fn cosine_identity_is_one() {
        let v = vec![1.0, 2.0, 3.0];
        approx_eq(cosine_similarity(&v, &v), 1.0, 1e-6);
    }

    #[test]
    fn cosine_orthogonal_is_zero() {
        let a = vec![1.0, 0.0];
        let b = vec![0.0, 1.0];
        approx_eq(cosine_similarity(&a, &b), 0.0, 1e-6);
    }

    #[test]
    fn cosine_zero_norm_is_zero() {
        let a = vec![0.0, 0.0];
        let b = vec![1.0, 1.0];
        approx_eq(cosine_similarity(&a, &b), 0.0, 1e-6);
    }

    #[test]
    fn knn_orders_by_similarity_descending_then_doc_id() {
        let mut idx = MemoryVectorIndex::new(2);
        idx.add(1, vec![1.0, 0.0]);
        idx.add(2, vec![0.5, 0.5]);
        idx.add(3, vec![0.0, 1.0]);
        let pl = idx.search_knn(&[1.0, 0.0], 2);
        let docs: Vec<_> = pl.iter().map(|e| e.doc_id).collect();
        // posting list is doc_id-sorted but the top-2 should be {1, 2}.
        assert_eq!(docs, vec![1, 2]);
        let entry1 = pl.get_entry(1).unwrap();
        let entry2 = pl.get_entry(2).unwrap();
        assert!(entry1.payload.score > entry2.payload.score);
    }

    #[test]
    fn threshold_filters_below_cutoff() {
        let mut idx = MemoryVectorIndex::new(2);
        idx.add(1, vec![1.0, 0.0]);
        idx.add(2, vec![0.5, 0.5]);
        idx.add(3, vec![0.0, 1.0]);
        let pl = idx.search_threshold(&[1.0, 0.0], 0.5);
        let docs: Vec<_> = pl.iter().map(|e| e.doc_id).collect();
        assert_eq!(docs, vec![1, 2]);
    }

    #[test]
    fn delete_removes_vector() {
        let mut idx = MemoryVectorIndex::new(2);
        idx.add(1, vec![1.0, 0.0]);
        idx.delete(1);
        assert_eq!(idx.count(), 0);
    }
}
