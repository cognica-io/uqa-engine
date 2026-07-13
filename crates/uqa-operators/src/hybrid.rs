//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Hybrid text + vector operators and multi-signal log-odds fusion.

use std::collections::BTreeMap;
use std::sync::Arc;

use uqa_core::{
    DocId, FieldName, IndexStats, Payload, PostingEntry, PostingList, Predicate, Value,
};
use uqa_fusion::{
    AdaptiveLogOddsFusion as AdaptiveLogOddsFuser, LogOddsFusion, LogitGating,
    ProbabilisticBoolean, SignalQuality,
};

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
pub fn coverage_based_default(n_hits: usize, n_total: usize, floor: f64) -> f64 {
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
/// documents contribute zero gated logit, matching Lucene's sparse scorer.
pub struct LogOddsFusionOperator {
    pub signals: Vec<Arc<dyn Operator>>,
    pub alpha: f64,
    pub gating: LogitGating,
    pub weights: Option<Vec<f64>>,
    pub logit_min: Option<Vec<f64>>,
    pub logit_max: Option<Vec<f64>>,
    pub top_k: Option<usize>,
}

impl LogOddsFusionOperator {
    pub fn new(signals: Vec<Arc<dyn Operator>>, alpha: f64) -> Self {
        LogOddsFusion::new(alpha);
        Self {
            signals,
            alpha,
            gating: LogitGating::Softplus,
            weights: None,
            logit_min: None,
            logit_max: None,
            top_k: None,
        }
    }

    pub fn with_gating(mut self, gating: LogitGating) -> Self {
        self.gating = gating;
        self
    }

    pub fn with_weights(mut self, weights: Vec<f64>) -> Self {
        LogOddsFusion::new(self.alpha)
            .validate_configuration(
                self.signals.len(),
                Some(&weights),
                self.logit_min.as_deref(),
                self.logit_max.as_deref(),
            )
            .expect("log-odds fusion weights must be valid");
        self.weights = Some(weights);
        self
    }

    pub fn with_logit_normalization(mut self, logit_min: Vec<f64>, logit_max: Vec<f64>) -> Self {
        LogOddsFusion::new(self.alpha)
            .validate_configuration(
                self.signals.len(),
                self.weights.as_deref(),
                Some(&logit_min),
                Some(&logit_max),
            )
            .expect("log-odds fusion bounds must be valid");
        self.logit_min = Some(logit_min);
        self.logit_max = Some(logit_max);
        self
    }

    pub fn with_top_k(mut self, top_k: usize) -> Self {
        self.top_k = Some(top_k);
        self
    }
}

impl Operator for LogOddsFusionOperator {
    fn execute(&self, ctx: &ExecutionContext) -> PostingList {
        let mut fuser = LogOddsFusion::new(self.alpha);
        fuser.gating = self.gating;
        fuser
            .validate_configuration(
                self.signals.len(),
                self.weights.as_deref(),
                self.logit_min.as_deref(),
                self.logit_max.as_deref(),
            )
            .expect("log-odds fusion configuration must be valid");
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

        let active_signal_count = score_maps
            .iter()
            .filter(|scores| !scores.is_empty())
            .count();
        if active_signal_count == 1 {
            let result = posting_lists
                .into_iter()
                .find(|posting_list| !posting_list.is_empty())
                .expect("one active signal has a posting list");
            return match self.top_k {
                Some(k) => result.top_k(k),
                None => result,
            };
        }

        let mut entries = Vec::with_capacity(all_doc_ids.len());
        for doc_id in &all_doc_ids {
            let probabilities: Vec<Option<f64>> = score_maps
                .iter()
                .map(|scores| scores.get(doc_id).copied())
                .collect();
            let fused_score = fuser
                .fuse_configured(
                    &probabilities,
                    self.weights.as_deref(),
                    self.logit_min.as_deref(),
                    self.logit_max.as_deref(),
                )
                .expect("log-odds fusion configuration must be valid");
            entries.push(PostingEntry::new(*doc_id, Payload::with_score(fused_score)));
        }
        let result = PostingList::from_sorted_unchecked(entries);
        match self.top_k {
            Some(k) => result.top_k(k),
            None => result,
        }
    }

    fn cost_estimate(&self, stats: &IndexStats) -> f64 {
        self.signals.iter().map(|s| s.cost_estimate(stats)).sum()
    }
}

/// Probabilistic boolean fusion. Matches UQA behavior for
/// `ProbBoolFusionOperator`. Each signal must produce calibrated
/// probabilities in `(0, 1)`; missing documents fall back to a
/// coverage-based default. `mode = And` multiplies probabilities,
/// `mode = Or` uses inclusion-exclusion via [`ProbabilisticBoolean`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProbBoolMode {
    And,
    Or,
}

pub struct ProbBoolFusionOperator {
    pub signals: Vec<Arc<dyn Operator>>,
    pub mode: ProbBoolMode,
}

impl ProbBoolFusionOperator {
    pub fn new(signals: Vec<Arc<dyn Operator>>, mode: ProbBoolMode) -> Self {
        Self { signals, mode }
    }
}

impl Operator for ProbBoolFusionOperator {
    fn execute(&self, ctx: &ExecutionContext) -> PostingList {
        let posting_lists: Vec<PostingList> =
            self.signals.iter().map(|sig| sig.execute(ctx)).collect();
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
        let mut entries: Vec<PostingEntry> = Vec::with_capacity(num_docs);
        for doc_id in &all_doc_ids {
            let probs: Vec<f64> = score_maps
                .iter()
                .zip(&defaults)
                .map(|(m, def)| m.get(doc_id).copied().unwrap_or(*def))
                .collect();
            let fused = match self.mode {
                ProbBoolMode::And => ProbabilisticBoolean::and(&probs),
                ProbBoolMode::Or => ProbabilisticBoolean::or(&probs),
            };
            entries.push(PostingEntry::new(*doc_id, Payload::with_score(fused)));
        }
        PostingList::from_sorted_unchecked(entries)
    }

    fn cost_estimate(&self, stats: &IndexStats) -> f64 {
        self.signals.iter().map(|s| s.cost_estimate(stats)).sum()
    }
}

/// Probabilistic NOT (`P(¬signal) = 1 - P(signal)`). Matches UQA behavior for
/// `ProbNotOperator`. Documents present in `signal` get
/// `1 - score`; documents missing from the signal but present in
/// the document store get `1 - default_prob`.
pub struct ProbNotOperator {
    pub signal: Arc<dyn Operator>,
    pub default_prob: f64,
}

impl ProbNotOperator {
    pub fn new(signal: Arc<dyn Operator>, default_prob: f64) -> Self {
        Self {
            signal,
            default_prob,
        }
    }
}

impl Operator for ProbNotOperator {
    fn execute(&self, ctx: &ExecutionContext) -> PostingList {
        let pl = self.signal.execute(ctx);
        let mut score_map: BTreeMap<DocId, f64> = BTreeMap::new();
        let mut all_ids: std::collections::BTreeSet<DocId> = std::collections::BTreeSet::new();
        for entry in &pl {
            score_map.insert(entry.doc_id, entry.payload.score);
            all_ids.insert(entry.doc_id);
        }
        if let Some(store) = ctx.document_store.as_ref() {
            for id in store.doc_ids() {
                all_ids.insert(id);
            }
        }
        let mut entries: Vec<PostingEntry> = Vec::with_capacity(all_ids.len());
        for doc_id in &all_ids {
            let p = score_map.get(doc_id).copied().unwrap_or(self.default_prob);
            entries.push(PostingEntry::new(*doc_id, Payload::with_score(1.0 - p)));
        }
        PostingList::from_sorted_unchecked(entries)
    }

    fn cost_estimate(&self, stats: &IndexStats) -> f64 {
        self.signal.cost_estimate(stats)
    }
}

/// `VE(V1, V2) = V1 AND NOT V2` — keeps documents from `positive`
/// that are dissimilar to `negative_op`'s query. Matches UQA behavior for
/// `VectorExclusionOperator`. The negative side is wired through a
/// [`VectorSimilarityOperator`] threshold so the caller decides what
/// counts as "too similar".
pub struct VectorExclusionOperator {
    pub positive: Arc<dyn Operator>,
    pub negative_op: VectorSimilarityOperator,
}

impl VectorExclusionOperator {
    pub fn new(
        positive: Arc<dyn Operator>,
        negative_vector: Vec<f32>,
        negative_threshold: f32,
        field: impl Into<FieldName>,
    ) -> Self {
        Self {
            positive,
            negative_op: VectorSimilarityOperator::new(negative_vector, negative_threshold, field),
        }
    }
}

impl Operator for VectorExclusionOperator {
    fn execute(&self, ctx: &ExecutionContext) -> PostingList {
        let positive_pl = self.positive.execute(ctx);
        let negative_pl = self.negative_op.execute(ctx);
        let negative_ids: std::collections::BTreeSet<DocId> =
            negative_pl.entries().iter().map(|e| e.doc_id).collect();
        let mut entries: Vec<PostingEntry> = Vec::new();
        for entry in positive_pl.entries() {
            if !negative_ids.contains(&entry.doc_id) {
                entries.push(entry.clone());
            }
        }
        PostingList::from_sorted_unchecked(entries)
    }

    fn cost_estimate(&self, stats: &IndexStats) -> f64 {
        self.positive.cost_estimate(stats) + self.negative_op.cost_estimate(stats)
    }
}

/// Facet counts conditioned on vector similarity. Matches UQA behavior for
/// `FacetVectorOperator`. The output rows are synthetic posting
/// entries — `doc_id` is a positional placeholder, `payload.score`
/// is the bucket count, and `payload.fields` carries the
/// `_facet_field` / `_facet_value` / `_facet_count` triple.
pub struct FacetVectorOperator {
    pub facet_field: String,
    pub vector_op: VectorSimilarityOperator,
    pub source: Option<Arc<dyn Operator>>,
}

impl FacetVectorOperator {
    pub fn new(
        facet_field: impl Into<String>,
        query_vector: Vec<f32>,
        threshold: f32,
        source: Option<Arc<dyn Operator>>,
    ) -> Self {
        Self {
            facet_field: facet_field.into(),
            // The UQA SQL contract defaults the field to "embedding".
            vector_op: VectorSimilarityOperator::new(query_vector, threshold, "embedding"),
            source,
        }
    }
}

impl Operator for FacetVectorOperator {
    fn execute(&self, ctx: &ExecutionContext) -> PostingList {
        let vector_pl = self.vector_op.execute(ctx);
        let vector_ids: std::collections::BTreeSet<DocId> =
            vector_pl.entries().iter().map(|e| e.doc_id).collect();
        let candidate_ids: Vec<DocId> = if let Some(src) = &self.source {
            src.execute(ctx)
                .entries()
                .iter()
                .filter(|e| vector_ids.contains(&e.doc_id))
                .map(|e| e.doc_id)
                .collect()
        } else {
            let mut v: Vec<DocId> = vector_ids.iter().copied().collect();
            v.sort_unstable();
            v
        };
        let Some(doc_store) = ctx.document_store.as_ref() else {
            return PostingList::new();
        };
        let mut value_counts: BTreeMap<String, u64> = BTreeMap::new();
        for doc_id in candidate_ids {
            if let Some(value) = doc_store.get_field(doc_id, &self.facet_field) {
                if !matches!(value, Value::Null) {
                    let key = value_to_facet_string(&value);
                    *value_counts.entry(key).or_insert(0) += 1;
                }
            }
        }
        let mut entries: Vec<PostingEntry> = Vec::with_capacity(value_counts.len());
        for (i, (value, count)) in value_counts.into_iter().enumerate() {
            let mut fields = BTreeMap::new();
            fields.insert(
                "_facet_field".to_string(),
                Value::Str(self.facet_field.clone()),
            );
            fields.insert("_facet_value".to_string(), Value::Str(value));
            fields.insert("_facet_count".to_string(), Value::Int(count as i64));
            entries.push(PostingEntry::new(
                i as DocId,
                Payload {
                    positions: Vec::new(),
                    score: count as f64,
                    fields,
                },
            ));
        }
        PostingList::from_sorted_unchecked(entries)
    }

    fn cost_estimate(&self, stats: &IndexStats) -> f64 {
        let mut base = self.vector_op.cost_estimate(stats);
        if let Some(src) = &self.source {
            base += src.cost_estimate(stats);
        }
        base
    }
}

fn value_to_facet_string(v: &Value) -> String {
    match v {
        Value::Str(s) => s.clone(),
        Value::Int(n) => n.to_string(),
        Value::Float(f) => format!("{f}"),
        Value::Bool(b) => b.to_string(),
        other => format!("{other:?}"),
    }
}

/// Adaptive log-odds fusion. Matches UQA behavior for
/// `AdaptiveLogOddsFusionOperator` — runs each signal, computes a
/// per-signal `SignalQuality` (coverage / variance / calibration
/// error), and combines through [`AdaptiveLogOddsFuser::fuse`].
pub struct AdaptiveLogOddsFusionOperator {
    pub signals: Vec<Arc<dyn Operator>>,
    pub base_alpha: f64,
    pub gating: Option<String>,
}

impl AdaptiveLogOddsFusionOperator {
    pub fn new(signals: Vec<Arc<dyn Operator>>, base_alpha: f64, gating: Option<String>) -> Self {
        Self {
            signals,
            base_alpha,
            gating,
        }
    }
}

impl Operator for AdaptiveLogOddsFusionOperator {
    fn execute(&self, ctx: &ExecutionContext) -> PostingList {
        let posting_lists: Vec<PostingList> =
            self.signals.iter().map(|sig| sig.execute(ctx)).collect();
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
        let qualities: Vec<SignalQuality> = score_maps
            .iter()
            .map(|smap| {
                let coverage = if num_docs > 0 {
                    smap.len() as f64 / num_docs as f64
                } else {
                    0.0
                };
                let scores: Vec<f64> = smap.values().copied().collect();
                let variance = if scores.len() > 1 {
                    let mean = scores.iter().sum::<f64>() / scores.len() as f64;
                    scores.iter().map(|s| (s - mean).powi(2)).sum::<f64>() / scores.len() as f64
                } else {
                    0.0
                };
                let mean_score = if scores.is_empty() {
                    0.5
                } else {
                    scores.iter().sum::<f64>() / scores.len() as f64
                };
                SignalQuality {
                    coverage_ratio: coverage,
                    score_variance: variance,
                    calibration_error: (mean_score - 0.5).abs(),
                }
            })
            .collect();
        let defaults: Vec<f64> = score_maps
            .iter()
            .map(|m| coverage_based_default(m.len(), num_docs, 0.01))
            .collect();
        let fusion = AdaptiveLogOddsFuser::new(self.base_alpha);
        let _ = &self.gating; // gating isn't wired into the Rust fuser yet.
        let mut entries: Vec<PostingEntry> = Vec::with_capacity(num_docs);
        for doc_id in &all_doc_ids {
            let probs: Vec<f64> = score_maps
                .iter()
                .zip(&defaults)
                .map(|(m, def)| m.get(doc_id).copied().unwrap_or(*def))
                .collect();
            let fused = fusion.fuse(&probs, &qualities).unwrap_or(0.5);
            entries.push(PostingEntry::new(*doc_id, Payload::with_score(fused)));
        }
        PostingList::from_sorted_unchecked(entries)
    }

    fn cost_estimate(&self, stats: &IndexStats) -> f64 {
        self.signals.iter().map(|s| s.cost_estimate(stats)).sum()
    }
}

/// Index-driven scan. Matches UQA behavior for `IndexScanOperator`. Wraps a
/// boxed [`uqa_storage::Index`] and runs `scan(predicate)` against
/// it. The optimiser's `apply_index_scan` rewrites a `Filter` into
/// this when an index covers the predicate.
pub struct IndexScanOperator {
    pub index: Arc<dyn uqa_storage::Index>,
    pub field: String,
    pub predicate: Predicate,
}

impl IndexScanOperator {
    pub fn new(
        index: Arc<dyn uqa_storage::Index>,
        field: impl Into<String>,
        predicate: Predicate,
    ) -> Self {
        Self {
            index,
            field: field.into(),
            predicate,
        }
    }
}

impl Operator for IndexScanOperator {
    fn execute(&self, _ctx: &ExecutionContext) -> PostingList {
        self.index.scan(&self.predicate)
    }

    fn cost_estimate(&self, _stats: &IndexStats) -> f64 {
        self.index.scan_cost(&self.predicate)
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
