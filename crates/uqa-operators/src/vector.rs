//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Vector similarity and KNN operators.

use std::sync::Arc;

use uqa_core::{FieldName, IndexStats, Payload, PostingEntry, PostingList};
use uqa_scoring::cosine_to_probability;
use uqa_storage::{StorageBackendError, StorageBackendResult};

use crate::base::{
    missing_backend, require_finite_score, ExecutionContext, Operator, OperatorResult,
};

/// `V_theta(q)`: returns documents with cosine similarity at least
/// `threshold` (Definition 3.1.2). Returns an empty posting list when the
/// field has no vector index registered.
pub struct VectorSimilarityOperator {
    pub query_vector: Vec<f32>,
    pub threshold: f32,
    pub field: FieldName,
}

impl VectorSimilarityOperator {
    pub fn new(query_vector: Vec<f32>, threshold: f32, field: impl Into<FieldName>) -> Self {
        Self {
            query_vector,
            threshold,
            field: field.into(),
        }
    }
}

impl Operator for VectorSimilarityOperator {
    fn execute(&self, ctx: &ExecutionContext) -> OperatorResult {
        validate_vector_query(&self.query_vector, "vector similarity")?;
        if !self.threshold.is_finite() || !(-1.0..=1.0).contains(&self.threshold) {
            return Err(StorageBackendError::Other(format!(
                "vector similarity threshold must be finite and in [-1, 1], got {}",
                self.threshold
            )));
        }
        ctx.vector_indexes
            .get(&self.field)
            .ok_or_else(|| missing_backend("vector-index", "vector similarity"))?
            .search_threshold(&self.query_vector, self.threshold)
    }

    fn cost_estimate(&self, stats: &IndexStats) -> f64 {
        let n = (stats.total_docs + 1) as f64;
        f64::from(stats.dimensions) * n.log2()
    }
}

/// `KNN_k(q)`: top-`k` nearest neighbors by cosine similarity
/// (Definition 3.1.3).
pub struct KNNOperator {
    pub query_vector: Vec<f32>,
    pub k: usize,
    pub field: FieldName,
}

impl KNNOperator {
    pub fn new(query_vector: Vec<f32>, k: usize, field: impl Into<FieldName>) -> Self {
        Self {
            query_vector,
            k,
            field: field.into(),
        }
    }
}

impl Operator for KNNOperator {
    fn execute(&self, ctx: &ExecutionContext) -> OperatorResult {
        validate_vector_query(&self.query_vector, "KNN search")?;
        ctx.vector_indexes
            .get(&self.field)
            .ok_or_else(|| missing_backend("vector-index", "KNN search"))?
            .search_knn(&self.query_vector, self.k)
    }

    fn cost_estimate(&self, stats: &IndexStats) -> f64 {
        let n = (stats.total_docs + 1) as f64;
        f64::from(stats.dimensions) * n.log2()
    }
}

/// Wraps a vector operator (KNN, threshold, ...) and rewrites each score
/// from cosine similarity in `[-1, 1]` to an evidence value in `[0, 1]` via
/// `(1 + score) / 2` (Definition 7.1.2, Paper 3). This is the
/// uncalibrated bridge between vector signals and evidence-combination
/// pipeline; calibrated alternatives (Paper 5) live in
/// `QueryPoolVectorScoreOperator`; reusable calibrated models live in
/// `uqa_scoring::VectorCalibrationModel`.
pub struct CosineProbabilityOperator {
    pub source: Arc<dyn Operator>,
}

impl CosineProbabilityOperator {
    pub fn new(source: Arc<dyn Operator>) -> Self {
        Self { source }
    }
}

impl Operator for CosineProbabilityOperator {
    fn execute(&self, ctx: &ExecutionContext) -> OperatorResult {
        let pl = self.source.execute(ctx)?;
        let mut entries = Vec::with_capacity(pl.len());
        for entry in pl.entries() {
            require_finite_score(entry.payload.score, "cosine probability projection")?;
            if !(-1.0..=1.0).contains(&entry.payload.score) {
                return Err(StorageBackendError::Other(format!(
                    "cosine probability projection requires scores in [-1, 1], got {}",
                    entry.payload.score
                )));
            }
            entries.push(PostingEntry::new(
                entry.doc_id,
                Payload {
                    positions: entry.payload.positions.clone(),
                    score: cosine_to_probability(entry.payload.score),
                    fields: entry.payload.fields.clone(),
                },
            ));
        }
        Ok(PostingList::from_sorted_unchecked(entries))
    }

    fn cost_estimate(&self, stats: &IndexStats) -> f64 {
        self.source.cost_estimate(stats)
    }
}

fn validate_vector_query(query: &[f32], operation: &str) -> StorageBackendResult<()> {
    if query.is_empty() {
        return Err(StorageBackendError::Other(format!(
            "{operation} requires a non-empty query vector"
        )));
    }
    if query.iter().any(|component| !component.is_finite()) {
        return Err(StorageBackendError::Other(format!(
            "{operation} query vector must contain only finite values"
        )));
    }
    Ok(())
}
