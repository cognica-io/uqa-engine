//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Vector index abstraction and an in-memory brute-force fallback.
//!
//! Operators (`KNNOperator`, `VectorSimilarityOperator`,
//! `QueryPoolVectorScoreOperator`) depend only on this trait. IVF and HNSW
//! backends slot in by implementing the same surface.

use std::collections::BTreeMap;
use std::sync::Arc;

use uqa_core::{DocId, Payload, PostingEntry, PostingList};

use crate::{StorageBackendError, StorageBackendResult};

mod config;

pub use config::{HNSWIndexParams, IVFIndexParams, VectorIndexOpenMode, VectorIndexSpec};

pub(crate) fn validate_vector_values(dimensions: u32, vector: &[f32]) -> StorageBackendResult<()> {
    let dimensions = usize::try_from(dimensions).map_err(|_| {
        StorageBackendError::Other(format!(
            "vector dimension {dimensions} exceeds the platform usize range"
        ))
    })?;
    if vector.len() != dimensions {
        return Err(StorageBackendError::Other(format!(
            "vector dimension mismatch: expected {dimensions}, got {}",
            vector.len()
        )));
    }
    if let Some((index, value)) = vector
        .iter()
        .copied()
        .enumerate()
        .find(|(_, value)| !value.is_finite())
    {
        return Err(StorageBackendError::Other(format!(
            "vector component {index} must be finite, got {value}"
        )));
    }
    Ok(())
}

fn checked_vector_count(counts: impl IntoIterator<Item = usize>) -> StorageBackendResult<usize> {
    counts.into_iter().try_fold(0_usize, |total, count| {
        total
            .checked_add(count)
            .ok_or_else(|| StorageBackendError::Other("vector count overflow".into()))
    })
}

fn validate_threshold(threshold: f32) -> StorageBackendResult<()> {
    if threshold.is_finite() {
        Ok(())
    } else {
        Err(StorageBackendError::Other(format!(
            "vector similarity threshold must be finite, got {threshold}"
        )))
    }
}

pub(crate) fn select_top_k_scored(scored: &mut Vec<(DocId, f32)>, k: usize) {
    if scored.len() > k {
        scored.select_nth_unstable_by(k, |a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        scored.truncate(k);
    }
}

/// Collapse tensor-vector scores to the best score for each document without
/// allocating one tree node per candidate. The final sort performed by each
/// caller restores the posting-list invariant after top-k selection.
pub(crate) fn deduplicate_scored_by_doc(scored: &mut Vec<(DocId, f32)>) {
    if scored.len() < 2 {
        return;
    }
    scored.sort_unstable_by_key(|(doc_id, _)| *doc_id);
    let mut write = 1;
    for read in 1..scored.len() {
        let (doc_id, score) = scored[read];
        if scored[write - 1].0 == doc_id {
            scored[write - 1].1 = scored[write - 1].1.max(score);
        } else {
            scored[write] = (doc_id, score);
            write += 1;
        }
    }
    scored.truncate(write);
}

pub(crate) fn vector_norm(vector: &[f32]) -> f32 {
    let mut squared_norm = 0.0_f32;
    for value in vector {
        squared_norm += value * value;
    }
    squared_norm.sqrt()
}

/// Cosine similarity when both vector norms were computed once outside the
/// candidate loop. This preserves the raw-vector score while avoiding two
/// norm reductions and two square roots for every candidate.
pub(crate) fn cosine_similarity_with_norms(a: &[f32], b: &[f32], norm_a: f32, norm_b: f32) -> f32 {
    if a.len() != b.len() || a.is_empty() || norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    let mut dot = 0.0_f32;
    for (x, y) in a.iter().zip(b) {
        dot += x * y;
    }
    dot / (norm_a * norm_b)
}

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
    fn index_kind(&self) -> &'static str {
        "vector"
    }
    fn add(&mut self, doc_id: DocId, vector: Vec<f32>) -> StorageBackendResult<()>;
    fn add_many(&mut self, doc_id: DocId, vectors: Vec<Vec<f32>>) -> StorageBackendResult<()>;
    fn delete(&mut self, doc_id: DocId) -> StorageBackendResult<()>;
    fn clear(&mut self) -> StorageBackendResult<()>;
    fn search_knn(&self, query: &[f32], k: usize) -> StorageBackendResult<PostingList>;
    fn search_threshold(&self, query: &[f32], threshold: f32) -> StorageBackendResult<PostingList>;
    fn count(&self) -> StorageBackendResult<usize>;

    /// Build any auxiliary physical metadata required by this index from its
    /// current vector contents. Brute-force indexes need no extra work;
    /// persistent IVF and HNSW implementations use this during explicit
    /// index creation, while restore paths deliberately skip it.
    fn initialize(&mut self) -> StorageBackendResult<()> {
        Ok(())
    }

    /// Read-only handle suitable for an `ExecutionContext`.
    fn snapshot(&self) -> StorageBackendResult<Arc<dyn VectorIndex>>;

    /// Independent writable copy used by in-memory engine rollback. The
    /// default keeps third-party and persistent implementations source
    /// compatible; only indexes hosted by a memory engine need to support it.
    fn writable_snapshot(&self) -> StorageBackendResult<Box<dyn VectorIndex>> {
        Err(StorageBackendError::Other(
            "writable vector-index snapshots are not supported by this backend".into(),
        ))
    }
}

#[derive(Debug, Clone)]
pub struct MemoryVectorIndex {
    dimensions: u32,
    vectors: BTreeMap<DocId, Vec<Vec<f32>>>,
}

impl MemoryVectorIndex {
    pub fn new(dimensions: u32) -> Self {
        Self {
            dimensions,
            vectors: BTreeMap::new(),
        }
    }

    pub fn vectors(&self) -> &BTreeMap<DocId, Vec<Vec<f32>>> {
        &self.vectors
    }
}

impl VectorIndex for MemoryVectorIndex {
    fn dimensions(&self) -> u32 {
        self.dimensions
    }

    fn index_kind(&self) -> &'static str {
        "memory-bruteforce"
    }

    fn add(&mut self, doc_id: DocId, vector: Vec<f32>) -> StorageBackendResult<()> {
        self.validate_dimensions(&vector)?;
        self.vectors.insert(doc_id, vec![vector]);
        Ok(())
    }

    fn add_many(&mut self, doc_id: DocId, vectors: Vec<Vec<f32>>) -> StorageBackendResult<()> {
        for vector in &vectors {
            self.validate_dimensions(vector)?;
        }
        if vectors.is_empty() {
            self.vectors.remove(&doc_id);
        } else {
            self.vectors.insert(doc_id, vectors);
        }
        Ok(())
    }

    fn delete(&mut self, doc_id: DocId) -> StorageBackendResult<()> {
        self.vectors.remove(&doc_id);
        Ok(())
    }

    fn clear(&mut self) -> StorageBackendResult<()> {
        self.vectors.clear();
        Ok(())
    }

    /// Brute-force top-k by cosine similarity, descending.
    fn search_knn(&self, query: &[f32], k: usize) -> StorageBackendResult<PostingList> {
        self.validate_dimensions(query)?;
        if k == 0 || self.vectors.is_empty() {
            return Ok(PostingList::new());
        }
        let mut scored: Vec<(DocId, f32)> = self
            .vectors
            .iter()
            .filter_map(|(&doc_id, vectors)| best_vector_score(query, vectors).map(|s| (doc_id, s)))
            .collect();
        select_top_k_scored(&mut scored, k);
        // The output of `top_k` is re-sorted by doc_id ascending so the
        // posting list invariant holds; the score lives in the payload.
        scored.sort_by_key(|(id, _)| *id);
        let entries = scored
            .into_iter()
            .map(|(doc_id, sim)| PostingEntry::new(doc_id, Payload::with_score(f64::from(sim))))
            .collect::<Vec<_>>();
        Ok(PostingList::from_sorted_unchecked(entries))
    }

    /// Brute-force threshold scan: keep all docs with `cosine >= threshold`.
    fn search_threshold(&self, query: &[f32], threshold: f32) -> StorageBackendResult<PostingList> {
        self.validate_dimensions(query)?;
        validate_threshold(threshold)?;
        let mut entries: Vec<PostingEntry> = self
            .vectors
            .iter()
            .filter_map(|(&doc_id, vectors)| {
                let sim = best_vector_score(query, vectors)?;
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
        Ok(PostingList::from_sorted_unchecked(entries))
    }

    fn count(&self) -> StorageBackendResult<usize> {
        checked_vector_count(self.vectors.values().map(Vec::len))
    }

    fn snapshot(&self) -> StorageBackendResult<Arc<dyn VectorIndex>> {
        Ok(Arc::new(self.clone()))
    }

    fn writable_snapshot(&self) -> StorageBackendResult<Box<dyn VectorIndex>> {
        Ok(Box::new(self.clone()))
    }
}

impl MemoryVectorIndex {
    fn validate_dimensions(&self, vector: &[f32]) -> StorageBackendResult<()> {
        validate_vector_values(self.dimensions, vector)
    }
}

fn best_vector_score(query: &[f32], vectors: &[Vec<f32>]) -> Option<f32> {
    vectors
        .iter()
        .map(|vector| cosine_similarity(query, vector))
        .max_by(f32::total_cmp)
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
    fn vector_count_overflow_is_reported() {
        let error = checked_vector_count([usize::MAX, 1]).unwrap_err();
        assert!(error.to_string().contains("vector count overflow"));
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
        idx.add(1, vec![1.0, 0.0]).unwrap();
        idx.add(2, vec![0.5, 0.5]).unwrap();
        idx.add(3, vec![0.0, 1.0]).unwrap();
        let pl = idx.search_knn(&[1.0, 0.0], 2).unwrap();
        let docs: Vec<_> = pl.iter().map(|e| e.doc_id).collect();
        // posting list is doc_id-sorted but the top-2 should be {1, 2}.
        assert_eq!(docs, vec![1, 2]);
        let entry1 = pl.get_entry(1).unwrap();
        let entry2 = pl.get_entry(2).unwrap();
        assert!(entry1.payload.score > entry2.payload.score);
    }

    #[test]
    fn partial_top_k_keeps_deterministic_doc_id_ties() {
        let mut scored = vec![(10, 0.5), (3, 0.9), (1, 0.9), (8, 0.7), (2, 0.1)];
        select_top_k_scored(&mut scored, 2);
        scored.sort_by_key(|(doc_id, _)| *doc_id);
        assert_eq!(scored, vec![(1, 0.9), (3, 0.9)]);
    }

    #[test]
    fn precomputed_norm_cosine_matches_reference_bits() {
        let a = [0.25, -3.0, 1.5, 8.0];
        let b = [2.0, 0.75, -4.0, 0.5];
        let expected = cosine_similarity(&a, &b);
        let actual = cosine_similarity_with_norms(&a, &b, vector_norm(&a), vector_norm(&b));
        assert_eq!(actual.to_bits(), expected.to_bits());
    }

    #[test]
    fn score_deduplication_keeps_best_tensor_vector() {
        let mut scored = vec![(7, 0.3), (2, 0.8), (7, 0.9), (2, 0.4), (9, -0.2)];
        deduplicate_scored_by_doc(&mut scored);
        assert_eq!(scored, vec![(2, 0.8), (7, 0.9), (9, -0.2)]);
    }

    #[test]
    fn threshold_filters_below_cutoff() {
        let mut idx = MemoryVectorIndex::new(2);
        idx.add(1, vec![1.0, 0.0]).unwrap();
        idx.add(2, vec![0.5, 0.5]).unwrap();
        idx.add(3, vec![0.0, 1.0]).unwrap();
        let pl = idx.search_threshold(&[1.0, 0.0], 0.5).unwrap();
        let docs: Vec<_> = pl.iter().map(|e| e.doc_id).collect();
        assert_eq!(docs, vec![1, 2]);
    }

    #[test]
    fn delete_removes_vector() {
        let mut idx = MemoryVectorIndex::new(2);
        idx.add(1, vec![1.0, 0.0]).unwrap();
        idx.delete(1).unwrap();
        assert_eq!(idx.count().unwrap(), 0);
    }

    #[test]
    fn non_finite_vectors_queries_and_thresholds_are_errors() {
        let mut idx = MemoryVectorIndex::new(2);
        assert!(idx.add(1, vec![f32::NAN, 0.0]).is_err());
        idx.add(1, vec![1.0, 0.0]).unwrap();
        assert!(idx.search_knn(&[f32::INFINITY, 0.0], 1).is_err());
        assert!(idx
            .search_threshold(&[1.0, 0.0], f32::NEG_INFINITY)
            .is_err());
    }
}
