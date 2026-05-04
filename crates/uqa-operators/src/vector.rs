//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Vector similarity and KNN operators.

use std::sync::Arc;

use uqa_core::{FieldName, IndexStats, PostingList};
use uqa_scoring::cosine_to_probability;

use crate::base::{ExecutionContext, Operator};

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
    fn execute(&self, ctx: &ExecutionContext) -> PostingList {
        match ctx.vector_indexes.get(&self.field) {
            Some(idx) => idx.search_threshold(&self.query_vector, self.threshold),
            None => PostingList::new(),
        }
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
    fn execute(&self, ctx: &ExecutionContext) -> PostingList {
        match ctx.vector_indexes.get(&self.field) {
            Some(idx) => idx.search_knn(&self.query_vector, self.k),
            None => PostingList::new(),
        }
    }

    fn cost_estimate(&self, stats: &IndexStats) -> f64 {
        let n = (stats.total_docs + 1) as f64;
        f64::from(stats.dimensions) * n.log2()
    }
}

/// Wraps a vector operator (KNN, threshold, ...) and rewrites each score
/// from cosine similarity in `[-1, 1]` to a probability in `(0, 1)` via
/// `(1 + score) / 2` (Definition 7.1.2, Paper 3). This is the
/// uncalibrated bridge between vector signals and the log-odds fusion
/// pipeline; calibrated alternatives (Paper 5) live in
/// `CalibratedVectorOperator`.
pub struct CosineProbabilityOperator {
    pub source: Arc<dyn Operator>,
}

impl CosineProbabilityOperator {
    pub fn new(source: Arc<dyn Operator>) -> Self {
        Self { source }
    }
}

impl Operator for CosineProbabilityOperator {
    fn execute(&self, ctx: &ExecutionContext) -> PostingList {
        let pl = self.source.execute(ctx);
        pl.with_scores(|e| cosine_to_probability(e.payload.score))
    }

    fn cost_estimate(&self, stats: &IndexStats) -> f64 {
        self.source.cost_estimate(stats)
    }
}
