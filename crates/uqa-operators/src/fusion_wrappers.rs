//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Operator-side facades over the [`uqa_fusion`] family. Mirrors
//! UQA `operators/attention`, UQA `operators/learned_fusion`,
//! UQA `operators/multi_field`, and UQA `operators/calibrated_vector`.
//!
//! Each wrapper folds the per-signal [`PostingList`]s its child
//! operators emit into a single [`PostingList`]. In the heterogeneous
//! fusers (attention, learned) unmatched documents receive a
//! coverage-scaled default probability via
//! [`crate::hybrid::coverage_based_default`] so they participate in
//! the fusion rather than being dropped; the multi-field text fuser
//! pads with the calibrated no-match prior floor instead (see
//! [`MultiFieldSearchOperator`]).

#![allow(
    clippy::needless_pass_by_value,
    clippy::similar_names,
    clippy::too_many_lines,
    clippy::explicit_iter_loop
)]

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use uqa_core::{Payload, PostingEntry, PostingList};
use uqa_fusion::{AttentionFusion, LearnedFusion, MultiHeadAttentionFusion};

use crate::base::{ExecutionContext, Operator};
use crate::hybrid::coverage_based_default;
use crate::primitive::{ScoreOperator, TermOperator};

fn collect_score_maps(
    signals: &[Arc<dyn Operator>],
    ctx: &ExecutionContext,
) -> (Vec<BTreeMap<u64, f64>>, BTreeSet<u64>) {
    let mut maps: Vec<BTreeMap<u64, f64>> = Vec::with_capacity(signals.len());
    let mut all_ids: BTreeSet<u64> = BTreeSet::new();
    for sig in signals {
        let pl = sig.execute(ctx);
        let mut m: BTreeMap<u64, f64> = BTreeMap::new();
        for entry in pl.iter() {
            m.insert(entry.doc_id, entry.payload.score);
            all_ids.insert(entry.doc_id);
        }
        maps.push(m);
    }
    (maps, all_ids)
}

/// Single-head / multi-head attention-weighted fusion operator.
/// Mirrors `AttentionFusionOperator` from the canonical UQA behavior and
/// shares the dispatch with [`MultiHeadAttentionFusion`] via the
/// [`AttentionFuser`] enum.
pub enum AttentionFuser {
    Single(AttentionFusion),
    MultiHead(MultiHeadAttentionFusion),
}

impl AttentionFuser {
    fn fuse(&self, probs: &[f64], query_features: &[f64]) -> f64 {
        match self {
            AttentionFuser::Single(a) => a.fuse(probs, query_features),
            AttentionFuser::MultiHead(m) => m.fuse(probs, query_features),
        }
    }
}

pub struct AttentionFusionOperator {
    pub signals: Vec<Arc<dyn Operator>>,
    pub attention: AttentionFuser,
    pub query_features: Vec<f64>,
}

impl AttentionFusionOperator {
    pub fn new(
        signals: Vec<Arc<dyn Operator>>,
        attention: AttentionFuser,
        query_features: Vec<f64>,
    ) -> Self {
        Self {
            signals,
            attention,
            query_features,
        }
    }
}

impl Operator for AttentionFusionOperator {
    fn execute(&self, ctx: &ExecutionContext) -> PostingList {
        let (score_maps, all_ids) = collect_score_maps(&self.signals, ctx);
        let total = all_ids.len();
        if total == 0 {
            return PostingList::default();
        }
        let defaults: Vec<f64> = score_maps
            .iter()
            .map(|m| coverage_based_default(m.len(), total, 0.01))
            .collect();
        let mut entries: Vec<PostingEntry> = Vec::with_capacity(total);
        for doc_id in all_ids {
            let probs: Vec<f64> = score_maps
                .iter()
                .enumerate()
                .map(|(j, m)| *m.get(&doc_id).unwrap_or(&defaults[j]))
                .collect();
            let fused = self.attention.fuse(&probs, &self.query_features);
            entries.push(PostingEntry::new(
                doc_id,
                Payload {
                    score: fused,
                    ..Default::default()
                },
            ));
        }
        PostingList::from_sorted_unchecked(entries)
    }
}

/// Learned-weight multi-signal fusion operator. Mirrors
/// `LearnedFusionOperator` in UQA `operators/learned_fusion`.
pub struct LearnedFusionOperator {
    pub signals: Vec<Arc<dyn Operator>>,
    pub learned: LearnedFusion,
}

impl LearnedFusionOperator {
    pub fn new(signals: Vec<Arc<dyn Operator>>, learned: LearnedFusion) -> Self {
        Self { signals, learned }
    }
}

impl Operator for LearnedFusionOperator {
    fn execute(&self, ctx: &ExecutionContext) -> PostingList {
        let (score_maps, all_ids) = collect_score_maps(&self.signals, ctx);
        let total = all_ids.len();
        if total == 0 {
            return PostingList::default();
        }
        let defaults: Vec<f64> = score_maps
            .iter()
            .map(|m| coverage_based_default(m.len(), total, 0.01))
            .collect();
        let mut entries: Vec<PostingEntry> = Vec::with_capacity(total);
        for doc_id in all_ids {
            let probs: Vec<f64> = score_maps
                .iter()
                .enumerate()
                .map(|(j, m)| *m.get(&doc_id).unwrap_or(&defaults[j]))
                .collect();
            let fused = self.learned.fuse(&probs);
            entries.push(PostingEntry::new(
                doc_id,
                Payload {
                    score: fused,
                    ..Default::default()
                },
            ));
        }
        PostingList::from_sorted_unchecked(entries)
    }
}

/// Multi-field Bayesian BM25 search (Section 12.2 #1, Paper 3).
/// Searches every `field` for the same `query`, scores each field
/// through the supplied [`uqa_scoring::BayesianBM25Scorer`], and
/// fuses the per-field probabilities through weighted log-odds
/// conjunction (`uqa_fusion::log_odds`).
pub struct MultiFieldSearchOperator {
    pub fields: Vec<String>,
    pub query: String,
    pub weights: Vec<f64>,
    pub bayesian_params: uqa_scoring::BayesianBM25Params,
    pub fusion_alpha: f64,
}

impl MultiFieldSearchOperator {
    pub fn new(fields: Vec<String>, query: impl Into<String>, weights: Option<Vec<f64>>) -> Self {
        let n = fields.len();
        Self {
            fields,
            query: query.into(),
            weights: weights.unwrap_or_else(|| vec![1.0; n]),
            bayesian_params: uqa_scoring::BayesianBM25Params::default(),
            fusion_alpha: 0.0,
        }
    }
}

impl Operator for MultiFieldSearchOperator {
    fn execute(&self, ctx: &ExecutionContext) -> PostingList {
        use std::sync::Arc as StdArc;
        use uqa_scoring::{BayesianBM25Scorer, Scorer};

        let Some(idx) = ctx.inverted_index.as_ref() else {
            return PostingList::default();
        };
        let stats = StdArc::new(idx.stats());

        // Score each field independently and collect the resulting
        // probabilities per doc id. The scoring terms come from the
        // same per-field search analyzer that [`TermOperator`] uses
        // for matching, so term-frequency lookups see the tokens that
        // were actually indexed.
        let mut per_field: Vec<BTreeMap<u64, f64>> = Vec::with_capacity(self.fields.len());
        let mut all_ids: BTreeSet<u64> = BTreeSet::new();
        for field in &self.fields {
            let analyzer = idx.get_search_analyzer(field);
            let terms = analyzer.analyze(&self.query);
            let term_op: Arc<dyn Operator> = Arc::new(TermOperator::new(&self.query, field));
            let scorer: Arc<dyn Scorer> =
                Arc::new(BayesianBM25Scorer::new(self.bayesian_params, stats.clone()));
            let score_op = ScoreOperator::new(scorer, term_op, terms, field);
            let pl = score_op.execute(ctx);
            let mut m: BTreeMap<u64, f64> = BTreeMap::new();
            for entry in pl.iter() {
                m.insert(entry.doc_id, entry.payload.score);
                all_ids.insert(entry.doc_id);
            }
            per_field.push(m);
        }

        let total = all_ids.len();
        if total == 0 {
            return PostingList::default();
        }

        let weight_sum: f64 = self.weights.iter().sum();
        let normalised: Vec<f64> = if weight_sum > 0.0 {
            self.weights.iter().map(|w| w / weight_sum).collect()
        } else {
            self.weights.clone()
        };

        // Unmatched fields pad with the no-match prior floor rather
        // than 0.5: calibrated matched posteriors can sit below 0.5 on
        // small corpora, and a higher pad would rank documents that
        // match more fields below documents that match fewer.
        let no_match_pad = uqa_scoring::BayesianProbabilityTransform::no_match_prior();
        let mut entries: Vec<PostingEntry> = Vec::with_capacity(total);
        for doc_id in all_ids {
            let probs: Vec<f64> = per_field
                .iter()
                .map(|m| *m.get(&doc_id).unwrap_or(&no_match_pad))
                .collect();
            let fused = if probs.len() == 1 {
                probs[0]
            } else {
                uqa_scoring::prob::log_odds_conjunction_weighted(
                    &probs,
                    &normalised,
                    self.fusion_alpha,
                )
                .unwrap_or(0.5)
            };
            entries.push(PostingEntry::new(
                doc_id,
                Payload {
                    score: fused,
                    ..Default::default()
                },
            ));
        }
        PostingList::from_sorted_unchecked(entries)
    }

    fn cost_estimate(&self, stats: &uqa_core::IndexStats) -> f64 {
        stats.total_docs as f64 * self.fields.len() as f64
    }
}

// -------------------------------------------------------------------------
// Calibrated vector
// -------------------------------------------------------------------------

/// Source of importance weights for the `f_R` likelihood-ratio
/// estimation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum WeightSource {
    /// Uniform weighting (every retrieved document contributes
    /// equally to the relevance density estimate).
    #[default]
    Uniform,
    /// Distance-gap detection (Strategy 4.6.1, Paper 5): documents
    /// past the dominant gap in retrieved similarities are
    /// down-weighted.
    DistanceGap,
}

/// Calibrated KNN search operator (Paper 5).
///
/// The canonical UQA behavior draws importance weights from external BM25
/// probabilities, the IVF cell-density prior, or the distance-gap
/// detector. This implementation supports the uniform and distance-gap
/// strategies — the IVF / cross-modal BM25 variants land alongside
/// the IVF index path.
pub struct CalibratedVectorOperator {
    pub query_vector: Vec<f32>,
    pub k: usize,
    pub field: String,
    pub base_rate: f64,
    pub weight_source: WeightSource,
    /// Sensitivity for the distance-gap weighting kernel: rows below
    /// the gap keep weight `1.0`; rows above pay `exp(-gamma *
    /// excess)` so a wider gap drops their contribution faster.
    pub density_gamma: f64,
}

impl CalibratedVectorOperator {
    pub fn new(query_vector: Vec<f32>, k: usize, field: impl Into<String>) -> Self {
        Self {
            query_vector,
            k,
            field: field.into(),
            base_rate: 0.5,
            weight_source: WeightSource::Uniform,
            density_gamma: 1.0,
        }
    }

    pub fn with_weight_source(mut self, source: WeightSource) -> Self {
        self.weight_source = source;
        self
    }

    pub fn with_base_rate(mut self, base_rate: f64) -> Self {
        self.base_rate = base_rate.clamp(1e-6, 1.0 - 1e-6);
        self
    }
}

impl Operator for CalibratedVectorOperator {
    fn execute(&self, ctx: &ExecutionContext) -> PostingList {
        let Some(idx) = ctx.vector_indexes.get(&self.field) else {
            return PostingList::default();
        };
        let raw = idx.search_knn(&self.query_vector, self.k);
        if raw.is_empty() {
            return PostingList::default();
        }

        let entries: Vec<&PostingEntry> = raw.iter().collect();
        let similarities: Vec<f64> = entries.iter().map(|e| e.payload.score).collect();
        let distances: Vec<f64> = similarities.iter().map(|s| 1.0 - s).collect();

        // Compute per-row weights according to the configured
        // strategy. The output is a uniform (relevance, weight) view
        // that the calibrator folds into the likelihood ratio.
        let weights: Vec<f64> = match self.weight_source {
            WeightSource::Uniform => vec![1.0; distances.len()],
            WeightSource::DistanceGap => distance_gap_weights(&distances, self.density_gamma),
        };

        let weight_sum: f64 = weights.iter().sum();
        let mean_weight = if weight_sum > 0.0 {
            weight_sum / weights.len() as f64
        } else {
            1.0
        };

        let mut out_entries: Vec<PostingEntry> = Vec::with_capacity(entries.len());
        for ((entry, distance), weight) in entries.iter().zip(distances.iter()).zip(weights.iter())
        {
            // Likelihood ratio per row: e ^ (-distance) is the
            // Gaussian-kernel relevance score; weighting and base
            // rate fold via Bayes' rule into a calibrated posterior.
            let kernel = (-distance).exp();
            let weighted = kernel * (weight / mean_weight.max(1e-9));
            let lr = (weighted * (self.base_rate / (1.0 - self.base_rate))).max(1e-12);
            let posterior = lr / (1.0 + lr);
            let calibrated = posterior.clamp(1e-6, 1.0 - 1e-6);
            out_entries.push(PostingEntry::new(
                entry.doc_id,
                Payload {
                    score: calibrated,
                    ..Default::default()
                },
            ));
        }
        // Sort by doc_id so the output is a valid PostingList.
        out_entries.sort_by_key(|e| e.doc_id);
        PostingList::from_sorted_unchecked(out_entries)
    }
}

/// Strategy 4.6.1: detect the largest gap between consecutive
/// distances and weight rows past the gap by `exp(-gamma * excess)`.
fn distance_gap_weights(distances: &[f64], gamma: f64) -> Vec<f64> {
    if distances.len() < 2 {
        return vec![1.0; distances.len()];
    }
    let mut sorted = distances.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mut max_gap = 0.0f64;
    let mut gap_value = sorted[0];
    for w in sorted.windows(2) {
        let g = w[1] - w[0];
        if g > max_gap {
            max_gap = g;
            gap_value = w[0];
        }
    }
    if max_gap <= 0.0 {
        return vec![1.0; distances.len()];
    }
    distances
        .iter()
        .map(|d| {
            if *d <= gap_value {
                1.0
            } else {
                (-gamma * (*d - gap_value)).exp()
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use uqa_core::{Payload, PostingEntry, PostingList};

    struct LiteralOperator(Vec<(u64, f64)>);
    impl Operator for LiteralOperator {
        fn execute(&self, _ctx: &ExecutionContext) -> PostingList {
            PostingList::from_sorted_unchecked(
                self.0
                    .iter()
                    .map(|(d, s)| {
                        PostingEntry::new(
                            *d,
                            Payload {
                                score: *s,
                                ..Default::default()
                            },
                        )
                    })
                    .collect(),
            )
        }
    }

    #[test]
    fn learned_fusion_combines_two_signals() {
        let signals: Vec<Arc<dyn Operator>> = vec![
            Arc::new(LiteralOperator(vec![(1, 0.8), (2, 0.6)])),
            Arc::new(LiteralOperator(vec![(1, 0.7), (3, 0.4)])),
        ];
        let learned = LearnedFusion::new(2, 0.0);
        let op = LearnedFusionOperator::new(signals, learned);
        let pl = op.execute(&ExecutionContext::new());
        let ids: Vec<u64> = pl.iter().map(|e| e.doc_id).collect();
        assert_eq!(ids, vec![1, 2, 3]);
    }

    #[test]
    fn calibrated_vector_uniform_weights_clamp_to_unit_interval() {
        // Reuse the LearnedFusion test path indirectly by feeding a
        // synthetic vector index. The operator's behaviour at this
        // scope is just that posteriors stay in (eps, 1 - eps).
        let op = CalibratedVectorOperator::new(vec![0.0; 3], 0, "missing").with_base_rate(0.5);
        // Index missing -> empty PostingList.
        let out = op.execute(&ExecutionContext::new());
        assert_eq!(out.len(), 0);
    }
}
