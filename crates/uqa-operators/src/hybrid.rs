//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Hybrid text + vector operators and multi-signal log-odds fusion.

use std::collections::BTreeMap;
use std::sync::Arc;

use uqa_core::{DocId, FieldName, IndexStats, Payload, PostingEntry, PostingList};
use uqa_scoring::log_odds_conjunction;

use crate::base::{ExecutionContext, Operator};
use crate::primitive::TermOperator;
use crate::vector::VectorSimilarityOperator;

/// Default probability for documents missing from a signal, interpolated
/// by the signal's coverage (Section 5, Paper 3 / Section 4, Paper 4):
///
/// `default = 0.5 * (1 - r) + floor * r`
///
/// where `r = n_hits / n_total`. A signal that returns nothing reports
/// neutral evidence (0.5, logit 0); a signal that covers everything
/// flags absence as strong negative evidence (= floor, default 0.01).
fn coverage_based_default(n_hits: usize, n_total: usize, floor: f64) -> f64 {
    if n_total == 0 {
        return 0.5;
    }
    let r = n_hits as f64 / n_total as f64;
    f64::midpoint(1.0 - r, 0.0) + floor * r
}

/// `Hybrid_{t, q, theta} = T(t) AND V_theta(q)` (Definition 3.3.1).
pub struct HybridTextVectorOperator {
    term_op: TermOperator,
    vector_op: VectorSimilarityOperator,
}

impl HybridTextVectorOperator {
    pub fn new(
        term: impl Into<String>,
        text_field: impl Into<FieldName>,
        query_vector: Vec<f32>,
        threshold: f32,
        vector_field: impl Into<FieldName>,
    ) -> Self {
        Self {
            term_op: TermOperator::new(term, text_field),
            vector_op: VectorSimilarityOperator::new(query_vector, threshold, vector_field),
        }
    }
}

impl Operator for HybridTextVectorOperator {
    fn execute(&self, ctx: &ExecutionContext) -> PostingList {
        self.term_op
            .execute(ctx)
            .intersect(&self.vector_op.execute(ctx))
    }

    fn cost_estimate(&self, stats: &IndexStats) -> f64 {
        self.term_op
            .cost_estimate(stats)
            .min(self.vector_op.cost_estimate(stats))
    }
}

/// `SemanticFilter_{q, theta, L} = L AND V_theta(q)` (Definition 3.3.4).
pub struct SemanticFilterOperator {
    pub source: Arc<dyn Operator>,
    pub vector_op: VectorSimilarityOperator,
}

impl SemanticFilterOperator {
    pub fn new(source: Arc<dyn Operator>, vector_op: VectorSimilarityOperator) -> Self {
        Self { source, vector_op }
    }
}

impl Operator for SemanticFilterOperator {
    fn execute(&self, ctx: &ExecutionContext) -> PostingList {
        self.source
            .execute(ctx)
            .intersect(&self.vector_op.execute(ctx))
    }

    fn cost_estimate(&self, stats: &IndexStats) -> f64 {
        self.source
            .cost_estimate(stats)
            .min(self.vector_op.cost_estimate(stats))
    }
}

/// Multi-signal fusion via log-odds conjunction (Section 4, Paper 4).
///
/// Each signal must produce calibrated probabilities in `(0, 1)`. Missing
/// documents fall back to a coverage-based default (see the private
/// `coverage_based_default` helper).
pub struct LogOddsFusionOperator {
    pub signals: Vec<Arc<dyn Operator>>,
    pub alpha: f64,
}

impl LogOddsFusionOperator {
    pub fn new(signals: Vec<Arc<dyn Operator>>, alpha: f64) -> Self {
        Self { signals, alpha }
    }
}

impl Operator for LogOddsFusionOperator {
    fn execute(&self, ctx: &ExecutionContext) -> PostingList {
        let posting_lists: Vec<PostingList> =
            self.signals.iter().map(|sig| sig.execute(ctx)).collect();

        // Build per-signal score maps and the universal doc id set.
        let mut all_doc_ids: std::collections::BTreeSet<DocId> = std::collections::BTreeSet::new();
        let score_maps: Vec<BTreeMap<DocId, f64>> = posting_lists
            .iter()
            .map(|pl| {
                let mut smap = BTreeMap::new();
                for entry in pl {
                    smap.insert(entry.doc_id, entry.payload.score);
                    all_doc_ids.insert(entry.doc_id);
                }
                smap
            })
            .collect();

        if all_doc_ids.is_empty() {
            return PostingList::new();
        }

        let num_docs = all_doc_ids.len();
        let defaults: Vec<f64> = score_maps
            .iter()
            .map(|m| coverage_based_default(m.len(), num_docs, 0.01))
            .collect();

        let mut entries = Vec::with_capacity(num_docs);
        let n_signals = self.signals.len();
        for doc_id in &all_doc_ids {
            let probs: Vec<f64> = score_maps
                .iter()
                .zip(&defaults)
                .map(|(m, def)| m.get(doc_id).copied().unwrap_or(*def))
                .collect();
            let fused = if n_signals == 1 {
                probs[0]
            } else {
                log_odds_conjunction(&probs, self.alpha)
            };
            entries.push(PostingEntry::new(*doc_id, Payload::with_score(fused)));
        }
        PostingList::from_sorted_unchecked(entries)
    }

    fn cost_estimate(&self, stats: &IndexStats) -> f64 {
        self.signals.iter().map(|s| s.cost_estimate(stats)).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coverage_default_neutral_at_zero_coverage() {
        let d = coverage_based_default(0, 0, 0.01);
        assert!((d - 0.5).abs() < 1e-12);
    }

    #[test]
    fn coverage_default_at_floor_at_full_coverage() {
        let d = coverage_based_default(100, 100, 0.01);
        assert!((d - 0.01).abs() < 1e-12);
    }

    #[test]
    fn coverage_default_interpolates() {
        let d = coverage_based_default(50, 100, 0.01);
        // r = 0.5 -> 0.5 * 0.5 + 0.01 * 0.5 = 0.255
        assert!((d - 0.255).abs() < 1e-12);
    }
}
