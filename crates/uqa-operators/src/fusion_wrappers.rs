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
//! fusers (attention, learned), unmatched documents receive a
//! coverage-scaled default probability via
//! [`crate::hybrid::coverage_based_default`] so they participate in
//! the fusion rather than being dropped. The multi-field text fuser uses
//! Lucene-style sparse absence: an unmatched field contributes zero.

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
use uqa_scoring::VectorProbabilityTransform;
use uqa_storage::{StorageBackendError, StorageBackendResult};

use crate::base::{
    missing_backend, require_finite_score, require_probability, ExecutionContext, Operator,
    OperatorResult,
};
use crate::hybrid::coverage_based_default;
use crate::primitive::{ScoreOperator, TermOperator};

type ScoreMap = BTreeMap<u64, f64>;
type CollectedScores = (Vec<ScoreMap>, BTreeSet<u64>);

fn collect_score_maps(
    signals: &[Arc<dyn Operator>],
    ctx: &ExecutionContext,
) -> StorageBackendResult<CollectedScores> {
    let mut maps: Vec<ScoreMap> = Vec::with_capacity(signals.len());
    let mut all_ids: BTreeSet<u64> = BTreeSet::new();
    for sig in signals {
        let pl = sig.execute(ctx)?;
        let mut m: BTreeMap<u64, f64> = BTreeMap::new();
        for entry in pl.iter() {
            require_probability(entry.payload.score, "learned/attention fusion")?;
            m.insert(entry.doc_id, entry.payload.score);
            all_ids.insert(entry.doc_id);
        }
        maps.push(m);
    }
    Ok((maps, all_ids))
}

fn require_single_active_evidence(probabilities: &[Option<f64>]) -> StorageBackendResult<f64> {
    probabilities
        .iter()
        .flatten()
        .next()
        .copied()
        .ok_or_else(|| {
            StorageBackendError::Other(
                "multi-field fusion invariant violated: the single active signal has no evidence"
                    .to_string(),
            )
        })
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
    fn validate_inputs(
        &self,
        signal_count: usize,
        query_feature_count: usize,
    ) -> Result<(), &'static str> {
        match self {
            AttentionFuser::Single(attention) => {
                attention.validate_inputs(signal_count, query_feature_count)
            }
            AttentionFuser::MultiHead(attention) => {
                attention.validate_inputs(signal_count, query_feature_count)
            }
        }
    }

    fn fuse_batch(
        &self,
        probabilities: &[Vec<f64>],
        query_features: &[f64],
    ) -> Result<Vec<f64>, &'static str> {
        match self {
            AttentionFuser::Single(attention) => {
                attention.fuse_batch(probabilities, query_features)
            }
            AttentionFuser::MultiHead(attention) => {
                attention.fuse_batch(probabilities, query_features)
            }
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
    fn execute(&self, ctx: &ExecutionContext) -> OperatorResult {
        self.attention
            .validate_inputs(self.signals.len(), self.query_features.len())
            .map_err(|error| StorageBackendError::Other(error.to_string()))?;
        let (score_maps, all_ids) = collect_score_maps(&self.signals, ctx)?;
        let total = all_ids.len();
        if total == 0 {
            return Ok(PostingList::default());
        }
        let defaults: Vec<f64> = score_maps
            .iter()
            .map(|m| coverage_based_default(m.len(), total, 0.01))
            .collect();
        let mut candidate_ids = Vec::with_capacity(total);
        let mut probabilities = Vec::with_capacity(total);
        for doc_id in all_ids {
            let probs: Vec<f64> = score_maps
                .iter()
                .enumerate()
                .map(|(j, m)| *m.get(&doc_id).unwrap_or(&defaults[j]))
                .collect();
            candidate_ids.push(doc_id);
            probabilities.push(probs);
        }
        let fused = self
            .attention
            .fuse_batch(&probabilities, &self.query_features)
            .map_err(|error| StorageBackendError::Other(error.to_string()))?;
        if fused.len() != candidate_ids.len() {
            return Err(StorageBackendError::Other(format!(
                "attention fusion returned {} scores for {} candidates",
                fused.len(),
                candidate_ids.len()
            )));
        }
        let entries = candidate_ids
            .into_iter()
            .zip(fused)
            .map(|(doc_id, score)| {
                PostingEntry::new(
                    doc_id,
                    Payload {
                        score,
                        ..Default::default()
                    },
                )
            })
            .collect();
        Ok(PostingList::from_sorted_unchecked(entries))
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
    fn execute(&self, ctx: &ExecutionContext) -> OperatorResult {
        self.learned
            .validate_inputs(self.signals.len())
            .map_err(|error| StorageBackendError::Other(error.to_string()))?;
        let (score_maps, all_ids) = collect_score_maps(&self.signals, ctx)?;
        let total = all_ids.len();
        if total == 0 {
            return Ok(PostingList::default());
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
            let fused = self
                .learned
                .fuse(&probs)
                .map_err(|error| StorageBackendError::Other(error.to_string()))?;
            entries.push(PostingEntry::new(
                doc_id,
                Payload {
                    score: fused,
                    ..Default::default()
                },
            ));
        }
        Ok(PostingList::from_sorted_unchecked(entries))
    }
}

/// Multi-field Bayesian BM25 search (Section 12.2 #1, Paper 3).
/// Searches every `field` with its corresponding query, scores each field
/// through a prior-free [`uqa_scoring::BayesianBM25Scorer`], and fuses
/// the per-field evidence through weighted robust positive-evidence pooling
/// (`uqa_fusion::positive_evidence`); the configured `base_rate` enters the
/// pool exactly once.
pub struct MultiFieldSearchOperator {
    pub fields: Vec<String>,
    pub queries: Vec<String>,
    pub weights: Vec<f64>,
    pub bayesian_params: uqa_scoring::BayesianBM25Params,
    pub fusion_alpha: f64,
}

impl MultiFieldSearchOperator {
    pub fn new(fields: Vec<String>, query: impl Into<String>, weights: Option<Vec<f64>>) -> Self {
        let n = fields.len();
        let query = query.into();
        Self {
            fields,
            queries: vec![query; n],
            weights: weights.unwrap_or_else(|| vec![1.0; n]),
            bayesian_params: uqa_scoring::BayesianBM25Params::default(),
            fusion_alpha: 0.5,
        }
    }

    pub fn with_queries(
        fields: Vec<String>,
        queries: Vec<String>,
        weights: Option<Vec<f64>>,
    ) -> Self {
        let n = fields.len();
        Self {
            fields,
            queries,
            weights: weights.unwrap_or_else(|| vec![1.0; n]),
            bayesian_params: uqa_scoring::BayesianBM25Params::default(),
            fusion_alpha: 0.5,
        }
    }
}

impl Operator for MultiFieldSearchOperator {
    fn execute(&self, ctx: &ExecutionContext) -> OperatorResult {
        use std::sync::Arc as StdArc;
        use uqa_scoring::{BayesianBM25Scorer, Scorer};

        let Some(idx) = ctx.inverted_index.as_ref() else {
            return Err(missing_backend("inverted-index", "multi-field search"));
        };
        if self.fields.is_empty() {
            return Err(StorageBackendError::Other(
                "multi-field search requires at least one field".to_string(),
            ));
        }
        if self.weights.len() != self.fields.len() {
            return Err(StorageBackendError::Other(format!(
                "multi-field search has {} fields but {} weights",
                self.fields.len(),
                self.weights.len()
            )));
        }
        if self.queries.len() != self.fields.len() {
            return Err(StorageBackendError::Other(format!(
                "multi-field search has {} fields but {} queries",
                self.fields.len(),
                self.queries.len()
            )));
        }
        // Score each field independently and collect the resulting
        // probabilities per doc id. The scoring terms come from the
        // same per-field search analyzer that [`TermOperator`] uses
        // for matching, so term-frequency lookups see the tokens that
        // were actually indexed.
        let mut per_field: Vec<BTreeMap<u64, f64>> = Vec::with_capacity(self.fields.len());
        let mut all_ids: BTreeSet<u64> = BTreeSet::new();
        for (field, query) in self.fields.iter().zip(&self.queries) {
            let analyzer = idx.get_search_analyzer(field);
            let terms = analyzer.analyze(query)?;
            let term_op: Arc<dyn Operator> = Arc::new(TermOperator::new(query, field));
            let scorer: Arc<dyn Scorer> = Arc::new(
                BayesianBM25Scorer::new(
                    self.bayesian_params
                        .scaled_for_query_terms(terms.len())
                        .evidence_params(),
                    StdArc::new(idx.field_stats(field)?),
                )
                .map_err(|error| StorageBackendError::Other(error.to_string()))?,
            );
            let score_op = ScoreOperator::new(scorer, term_op, terms, field);
            let pl = score_op.execute(ctx)?;
            let mut m: BTreeMap<u64, f64> = BTreeMap::new();
            for entry in pl.iter() {
                require_probability(entry.payload.score, "multi-field search")?;
                m.insert(entry.doc_id, entry.payload.score);
                all_ids.insert(entry.doc_id);
            }
            per_field.push(m);
        }

        let total = all_ids.len();
        if total == 0 {
            return Ok(PostingList::default());
        }

        let weight_sum: f64 = self.weights.iter().sum();
        let normalized: Vec<f64> = if weight_sum > 0.0
            && self
                .weights
                .iter()
                .all(|weight| weight.is_finite() && *weight >= 0.0)
        {
            self.weights.iter().map(|w| w / weight_sum).collect()
        } else {
            return Err(StorageBackendError::Other(
                "multi-field weights must be non-negative and have a positive finite sum"
                    .to_string(),
            ));
        };

        let active_field_count = per_field.iter().filter(|scores| !scores.is_empty()).count();
        let mut fusion = uqa_fusion::RobustPositiveEvidencePool::new(self.fusion_alpha)
            .map_err(|error| StorageBackendError::Other(error.to_string()))?;
        if self.bayesian_params.base_rate > 0.0 {
            fusion = fusion
                .with_base_rate(self.bayesian_params.base_rate)
                .map_err(|error| StorageBackendError::Other(error.to_string()))?;
        }
        let mut entries: Vec<PostingEntry> = Vec::with_capacity(total);
        for doc_id in all_ids {
            let probabilities: Vec<Option<f64>> = per_field
                .iter()
                .map(|scores| scores.get(&doc_id).copied())
                .collect();
            let fused = if active_field_count == 1 {
                // A de-facto single signal skips the weighted mean and
                // sqrt(n) scaling, but a configured prior still enters.
                let evidence = require_single_active_evidence(&probabilities)?;
                fusion.fuse(&[evidence])
            } else {
                fusion
                    .fuse_weighted_sparse(&probabilities, &normalized)
                    .map_err(|error| StorageBackendError::Other(error.to_string()))?
            };
            entries.push(PostingEntry::new(
                doc_id,
                Payload {
                    score: fused,
                    ..Default::default()
                },
            ));
        }
        Ok(PostingList::from_sorted_unchecked(entries))
    }

    fn cost_estimate(&self, stats: &uqa_core::IndexStats) -> f64 {
        stats.total_docs as f64 * self.fields.len() as f64
    }
}

// -------------------------------------------------------------------------
// Calibrated vector
// -------------------------------------------------------------------------

/// How the relevant-document sample (`f_R`) is split from the
/// retrieved pool before fitting the likelihood-ratio calibration.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RelevantSampleSplit {
    /// The closest quarter of the pool models the relevant density.
    #[default]
    TopQuartile,
    /// Strategy 4.6.1 (Paper 5): documents before the dominant gap in
    /// the sorted distances model the relevant density. Falls back to
    /// the top quartile when the pool has no positive gap.
    DistanceGap,
}

/// Calibrated KNN search operator (Paper 5, Theorem 3.1.1).
///
/// Fits the likelihood-ratio calibration from the retrieved pool at
/// query time: the head of the sorted distance distribution (per
/// [`RelevantSampleSplit`]) estimates the relevant density `f_R`, the
/// tail estimates the background density `f_G`, and each candidate's
/// posterior is `sigmoid(log(f_R(d) / f_G(d)) + logit(base_rate))` via
/// [`VectorProbabilityTransform`]. An uninformative pool (too small,
/// zero spread, or no head/tail separation) yields the prior for every
/// candidate instead of fabricating discrimination.
pub struct CalibratedVectorOperator {
    pub query_vector: Vec<f32>,
    pub k: usize,
    pub field: String,
    /// Relevance prior folded into the posterior. The default `0.5`
    /// contributes zero log-odds, so the output doubles as prior-free
    /// evidence for fusion-level priors.
    pub base_rate: f64,
    pub split: RelevantSampleSplit,
}

impl CalibratedVectorOperator {
    pub fn new(query_vector: Vec<f32>, k: usize, field: impl Into<String>) -> Self {
        Self {
            query_vector,
            k,
            field: field.into(),
            base_rate: 0.5,
            split: RelevantSampleSplit::default(),
        }
    }

    pub fn with_split(mut self, split: RelevantSampleSplit) -> Self {
        self.split = split;
        self
    }

    pub fn with_base_rate(mut self, base_rate: f64) -> Self {
        self.base_rate = base_rate;
        self
    }
}

impl Operator for CalibratedVectorOperator {
    fn execute(&self, ctx: &ExecutionContext) -> OperatorResult {
        if !self.base_rate.is_finite() || self.base_rate <= 0.0 || self.base_rate >= 1.0 {
            return Err(StorageBackendError::Other(format!(
                "calibrated vector base_rate must be finite and in (0, 1), got {}",
                self.base_rate
            )));
        }
        if self.query_vector.is_empty()
            || self
                .query_vector
                .iter()
                .any(|component| !component.is_finite())
        {
            return Err(StorageBackendError::Other(
                "calibrated vector search requires a non-empty finite query vector".to_string(),
            ));
        }
        let Some(idx) = ctx.vector_indexes.get(&self.field) else {
            return Err(missing_backend("vector-index", "calibrated vector search"));
        };
        let raw = idx.search_knn(&self.query_vector, self.k)?;
        if raw.is_empty() {
            return Ok(PostingList::default());
        }

        let mut distances = Vec::with_capacity(raw.len());
        for entry in raw.entries() {
            require_finite_score(entry.payload.score, "calibrated vector search")?;
            if !(-1.0..=1.0).contains(&entry.payload.score) {
                return Err(StorageBackendError::Other(format!(
                    "calibrated vector search requires cosine scores in [-1, 1], got {}",
                    entry.payload.score
                )));
            }
            distances.push(1.0 - entry.payload.score);
        }
        let calibrator = fit_pool_calibration(&distances, self.split, self.base_rate)?;

        let mut out_entries: Vec<PostingEntry> = Vec::with_capacity(raw.len());
        for (entry, distance) in raw.iter().zip(&distances) {
            let posterior = match calibrator.as_ref() {
                Some(transform) => transform
                    .calibrate_one(*distance)
                    .map_err(|error| StorageBackendError::Other(error.to_string()))?,
                None => self.base_rate,
            };
            out_entries.push(PostingEntry::new(
                entry.doc_id,
                Payload {
                    score: posterior.clamp(1e-6, 1.0 - 1e-6),
                    ..Default::default()
                },
            ));
        }
        // Sort by doc_id so the output is a valid PostingList.
        out_entries.sort_by_key(|e| e.doc_id);
        Ok(PostingList::from_sorted_unchecked(out_entries))
    }
}

/// Fit the two-Gaussian likelihood-ratio calibration from a retrieved
/// distance pool. Returns `None` when the pool carries no usable
/// relevance signal: fewer than two candidates, negligible spread, or
/// a head that is not closer than the tail.
pub fn fit_pool_calibration(
    distances: &[f64],
    split: RelevantSampleSplit,
    base_rate: f64,
) -> StorageBackendResult<Option<VectorProbabilityTransform>> {
    if !base_rate.is_finite() || base_rate <= 0.0 || base_rate >= 1.0 {
        return Err(StorageBackendError::Other(format!(
            "pool calibration base_rate must be finite and in (0, 1), got {base_rate}"
        )));
    }
    if distances.iter().any(|distance| !distance.is_finite()) {
        return Err(StorageBackendError::Other(
            "pool calibration distances must be finite".to_string(),
        ));
    }
    if distances.len() < 2 {
        return Ok(None);
    }
    let mut sorted = distances.to_vec();
    sorted.sort_by(f64::total_cmp);

    let head_len = match split {
        RelevantSampleSplit::TopQuartile => quartile_head(sorted.len()),
        RelevantSampleSplit::DistanceGap => {
            distance_gap_split(&sorted).unwrap_or_else(|| quartile_head(sorted.len()))
        }
    }
    .clamp(1, sorted.len() - 1);

    let mu_match = mean(&sorted[..head_len]);
    let mu_random = mean(&sorted[head_len..]);
    let sigma = standard_deviation(&sorted);
    if sigma <= f64::EPSILON || mu_random - mu_match <= f64::EPSILON {
        return Ok(None);
    }
    Ok(Some(
        VectorProbabilityTransform::new(mu_match, mu_random, sigma, base_rate)
            .map_err(|error| StorageBackendError::Other(error.to_string()))?,
    ))
}

fn quartile_head(pool_size: usize) -> usize {
    pool_size.div_ceil(4)
}

/// Strategy 4.6.1: index of the first element after the dominant gap
/// between consecutive sorted distances, provided a positive gap exists.
fn distance_gap_split(sorted: &[f64]) -> Option<usize> {
    let mut max_gap = 0.0f64;
    let mut split_index = None;
    for (index, window) in sorted.windows(2).enumerate() {
        let gap = window[1] - window[0];
        if gap > max_gap {
            max_gap = gap;
            split_index = Some(index + 1);
        }
    }
    split_index
}

fn mean(values: &[f64]) -> f64 {
    values.iter().sum::<f64>() / values.len() as f64
}

fn standard_deviation(values: &[f64]) -> f64 {
    let mu = mean(values);
    let variance = values
        .iter()
        .map(|value| {
            let difference = value - mu;
            difference * difference
        })
        .sum::<f64>()
        / values.len() as f64;
    variance.sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;
    use uqa_core::{Payload, PostingEntry, PostingList};

    struct LiteralOperator(Vec<(u64, f64)>);
    impl Operator for LiteralOperator {
        fn execute(&self, _ctx: &ExecutionContext) -> OperatorResult {
            Ok(PostingList::from_sorted_unchecked(
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
            ))
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
        let pl = op.execute(&ExecutionContext::new()).unwrap();
        let ids: Vec<u64> = pl.iter().map(|e| e.doc_id).collect();
        assert_eq!(ids, vec![1, 2, 3]);
    }

    #[test]
    fn missing_single_active_evidence_is_an_invariant_error() {
        let error = require_single_active_evidence(&[None, None]).unwrap_err();
        assert!(error.to_string().contains("single active signal"));
    }

    #[test]
    fn calibrated_vector_missing_index_is_an_execution_error() {
        let op = CalibratedVectorOperator::new(vec![0.0; 3], 0, "missing").with_base_rate(0.5);
        let error = op.execute(&ExecutionContext::new()).unwrap_err();
        assert!(error.to_string().contains("vector-index"));
    }

    #[test]
    fn pool_calibration_discriminates_head_from_tail() {
        let distances = [0.02, 0.05, 0.30, 0.35, 0.40, 0.45, 0.50, 0.55];
        let transform = fit_pool_calibration(&distances, RelevantSampleSplit::TopQuartile, 0.5)
            .expect("valid fit request")
            .expect("separated pool fits");
        let head = transform.calibrate_one(0.02).unwrap();
        let mid = transform.calibrate_one(0.30).unwrap();
        let tail = transform.calibrate_one(0.55).unwrap();
        assert!(head > mid && mid > tail, "{head} > {mid} > {tail}");
        assert!(head > 0.5, "head evidence must be positive, got {head}");
        assert!(tail < 0.5, "tail evidence must be negative, got {tail}");
    }

    #[test]
    fn pool_calibration_rejects_uninformative_pools() {
        assert!(
            fit_pool_calibration(&[0.3], RelevantSampleSplit::TopQuartile, 0.5)
                .unwrap()
                .is_none()
        );
        assert!(
            fit_pool_calibration(&[0.3, 0.3, 0.3, 0.3], RelevantSampleSplit::TopQuartile, 0.5)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn pool_calibration_rejects_invalid_numeric_inputs() {
        assert!(
            fit_pool_calibration(&[f64::NAN, 0.2], RelevantSampleSplit::TopQuartile, 0.5).is_err()
        );
        assert!(fit_pool_calibration(&[0.1, 0.2], RelevantSampleSplit::TopQuartile, 1.0).is_err());
    }

    #[test]
    fn distance_gap_split_finds_the_semantic_cliff() {
        let sorted = [0.05, 0.06, 0.07, 0.40, 0.42, 0.44];
        assert_eq!(distance_gap_split(&sorted), Some(3));
        assert_eq!(distance_gap_split(&[0.3, 0.3, 0.3]), None);
    }
}
