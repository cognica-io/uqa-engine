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
use uqa_storage::{StorageBackendError, StorageBackendResult};

use crate::base::{
    missing_backend, require_probability, ExecutionContext, Operator, OperatorResult,
};
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

fn validate_probability_postings(
    postings: &PostingList,
    operation: &str,
) -> StorageBackendResult<()> {
    for entry in postings.entries() {
        require_probability(entry.payload.score, operation)?;
    }
    Ok(())
}

/// Share of the adaptive weight mass distributed by gated-evidence
/// spread; the remainder stays uniform so no matching signal is ever
/// silenced entirely.
const ADAPTIVE_SPREAD_SHARE: f64 = 0.5;

/// Discrimination-based per-signal weights: each signal's weight blends
/// a uniform share with its share of the total gated-evidence spread
/// across its own matches. A signal that assigns every candidate the
/// same evidence carries no ranking information and sinks toward the
/// uniform floor. Returns `None` when no signal has measurable spread,
/// falling back to the unweighted mean.
fn adaptive_signal_weights(
    fuser: &LogOddsFusion,
    score_maps: &[BTreeMap<DocId, f64>],
) -> Option<Vec<f64>> {
    let spreads: Vec<f64> = score_maps
        .iter()
        .map(|scores| {
            if scores.len() < 2 {
                return 0.0;
            }
            let logits: Vec<f64> = scores
                .values()
                .map(|probability| fuser.gated_logit(*probability))
                .collect();
            let mean = logits.iter().sum::<f64>() / logits.len() as f64;
            let variance = logits
                .iter()
                .map(|logit| {
                    let difference = logit - mean;
                    difference * difference
                })
                .sum::<f64>()
                / logits.len() as f64;
            variance.sqrt()
        })
        .collect();
    let total: f64 = spreads.iter().sum();
    if total <= f64::EPSILON {
        return None;
    }
    let uniform = (1.0 - ADAPTIVE_SPREAD_SHARE) / score_maps.len() as f64;
    Some(
        spreads
            .iter()
            .map(|spread| uniform + ADAPTIVE_SPREAD_SHARE * spread / total)
            .collect(),
    )
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
    fn execute(&self, ctx: &ExecutionContext) -> OperatorResult {
        Ok(self
            .term_op
            .execute(ctx)?
            .intersect_owned(&self.vector_op.execute(ctx)?))
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
    fn execute(&self, ctx: &ExecutionContext) -> OperatorResult {
        Ok(self
            .source
            .execute(ctx)?
            .intersect_owned(&self.vector_op.execute(ctx)?))
    }

    fn cost_estimate(&self, stats: &IndexStats) -> f64 {
        self.source
            .cost_estimate(stats)
            .min(self.vector_op.cost_estimate(stats))
    }
}

/// Multi-signal fusion via log-odds conjunction (Section 4, Paper 4).
///
/// Each signal must produce prior-free evidence probabilities in
/// `(0, 1)`; a configured `base_rate` enters the fusion exactly once.
/// Missing documents contribute zero gated logit, and a signal with no
/// matches at all stays in the declared signal set as neutral evidence
/// (Lucene PR 16410 semantics: the clause count that governs `n^alpha`
/// and the uniform denominator never shrinks at execution time). The
/// default softplus gating floors match evidence at the prior;
/// `LogitGating::Pass` matches Lucene's signed default.
pub struct LogOddsFusionOperator {
    pub signals: Vec<Arc<dyn Operator>>,
    pub alpha: f64,
    pub gating: LogitGating,
    pub base_rate: Option<f64>,
    pub weights: Option<Vec<f64>>,
    /// Derive per-signal weights from each signal's gated-evidence
    /// spread over its matches (Theorem 8.3 reliability weighting,
    /// estimated unsupervised). Ignored when explicit `weights` are
    /// set.
    pub adaptive_weights: bool,
    pub logit_min: Option<Vec<f64>>,
    pub logit_max: Option<Vec<f64>>,
    pub top_k: Option<usize>,
}

impl LogOddsFusionOperator {
    pub fn new(signals: Vec<Arc<dyn Operator>>, alpha: f64) -> Self {
        Self {
            signals,
            alpha,
            gating: LogitGating::Softplus,
            base_rate: None,
            weights: None,
            adaptive_weights: false,
            logit_min: None,
            logit_max: None,
            top_k: None,
        }
    }

    pub fn with_adaptive_weights(mut self) -> Self {
        self.adaptive_weights = true;
        self
    }

    pub fn with_gating(mut self, gating: LogitGating) -> Self {
        self.gating = gating;
        self
    }

    /// Fusion-level relevance prior, applied exactly once.
    pub fn with_base_rate(mut self, base_rate: f64) -> Self {
        self.base_rate = Some(base_rate);
        self
    }

    pub fn with_weights(mut self, weights: Vec<f64>) -> Self {
        self.weights = Some(weights);
        self
    }

    pub fn with_logit_normalization(mut self, logit_min: Vec<f64>, logit_max: Vec<f64>) -> Self {
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
    fn execute(&self, ctx: &ExecutionContext) -> OperatorResult {
        if self.signals.is_empty() {
            return Err(StorageBackendError::Other(
                "log-odds fusion requires at least one signal".to_string(),
            ));
        }
        if !self.alpha.is_finite() || !(0.0..=1.0).contains(&self.alpha) {
            return Err(StorageBackendError::Other(format!(
                "log-odds fusion alpha must be finite and in [0, 1], got {}",
                self.alpha
            )));
        }
        if let Some(base_rate) = self.base_rate {
            if !base_rate.is_finite() || base_rate <= 0.0 || base_rate >= 1.0 {
                return Err(StorageBackendError::Other(format!(
                    "log-odds fusion base_rate must be finite and in (0, 1), got {base_rate}"
                )));
            }
        }
        let mut fuser = LogOddsFusion::new(self.alpha)
            .map_err(|error| StorageBackendError::Other(error.to_string()))?
            .with_logit_gating(self.gating);
        if let Some(base_rate) = self.base_rate {
            fuser = fuser
                .with_base_rate(base_rate)
                .map_err(|error| StorageBackendError::Other(error.to_string()))?;
        }
        fuser
            .validate_configuration(
                self.signals.len(),
                self.weights.as_deref(),
                self.logit_min.as_deref(),
                self.logit_max.as_deref(),
            )
            .map_err(|error| StorageBackendError::Other(error.to_string()))?;
        let posting_lists: Vec<PostingList> = self
            .signals
            .iter()
            .map(|sig| sig.execute(ctx))
            .collect::<StorageBackendResult<_>>()?;
        for posting_list in &posting_lists {
            validate_probability_postings(posting_list, "log-odds fusion")?;
        }

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
            return Ok(PostingList::new());
        }

        // A signal with no matches still contributes neutral evidence to
        // every document: the declared signal count governs `n^alpha`
        // and the uniform denominator, so a document's fused score
        // cannot depend on whether another signal happened to match
        // elsewhere (Lucene PR 16410 semantics).
        let weights = self.weights.clone().or_else(|| {
            if self.adaptive_weights {
                adaptive_signal_weights(&fuser, &score_maps)
            } else {
                None
            }
        });
        let mut entries = Vec::with_capacity(all_doc_ids.len());
        for doc_id in &all_doc_ids {
            let probabilities: Vec<Option<f64>> = score_maps
                .iter()
                .map(|scores| scores.get(doc_id).copied())
                .collect();
            let fused_score = fuser
                .fuse_configured(
                    &probabilities,
                    weights.as_deref(),
                    self.logit_min.as_deref(),
                    self.logit_max.as_deref(),
                )
                .map_err(|error| StorageBackendError::Other(error.to_string()))?;
            entries.push(PostingEntry::new(*doc_id, Payload::with_score(fused_score)));
        }
        let result = PostingList::from_sorted_unchecked(entries);
        Ok(match self.top_k {
            Some(k) => result.top_k(k),
            None => result,
        })
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
    fn execute(&self, ctx: &ExecutionContext) -> OperatorResult {
        if self.signals.is_empty() {
            return Err(StorageBackendError::Other(
                "probabilistic boolean fusion requires at least one signal".to_string(),
            ));
        }
        let posting_lists: Vec<PostingList> = self
            .signals
            .iter()
            .map(|sig| sig.execute(ctx))
            .collect::<StorageBackendResult<_>>()?;
        for posting_list in &posting_lists {
            validate_probability_postings(posting_list, "probabilistic boolean fusion")?;
        }
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
            return Ok(PostingList::new());
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
        Ok(PostingList::from_sorted_unchecked(entries))
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
    fn execute(&self, ctx: &ExecutionContext) -> OperatorResult {
        require_probability(self.default_prob, "probabilistic NOT default")?;
        let pl = self.signal.execute(ctx)?;
        validate_probability_postings(&pl, "probabilistic NOT")?;
        let mut score_map: BTreeMap<DocId, f64> = BTreeMap::new();
        let mut all_ids: std::collections::BTreeSet<DocId> = std::collections::BTreeSet::new();
        for entry in &pl {
            score_map.insert(entry.doc_id, entry.payload.score);
            all_ids.insert(entry.doc_id);
        }
        if let Some(store) = ctx.document_store.as_ref() {
            for id in store.doc_ids()? {
                all_ids.insert(id);
            }
        }
        let mut entries: Vec<PostingEntry> = Vec::with_capacity(all_ids.len());
        for doc_id in &all_ids {
            let p = score_map.get(doc_id).copied().unwrap_or(self.default_prob);
            entries.push(PostingEntry::new(*doc_id, Payload::with_score(1.0 - p)));
        }
        Ok(PostingList::from_sorted_unchecked(entries))
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
    fn execute(&self, ctx: &ExecutionContext) -> OperatorResult {
        let positive_pl = self.positive.execute(ctx)?;
        let negative_pl = self.negative_op.execute(ctx)?;
        let negative_ids: std::collections::BTreeSet<DocId> =
            negative_pl.entries().iter().map(|e| e.doc_id).collect();
        let mut entries: Vec<PostingEntry> = Vec::new();
        for entry in positive_pl.entries() {
            if !negative_ids.contains(&entry.doc_id) {
                entries.push(entry.clone());
            }
        }
        Ok(PostingList::from_sorted_unchecked(entries))
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
    fn execute(&self, ctx: &ExecutionContext) -> OperatorResult {
        let vector_pl = self.vector_op.execute(ctx)?;
        let vector_ids: std::collections::BTreeSet<DocId> =
            vector_pl.entries().iter().map(|e| e.doc_id).collect();
        let candidate_ids: Vec<DocId> = if let Some(src) = &self.source {
            src.execute(ctx)?
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
            return Err(missing_backend("document-store", "vector facet"));
        };
        let mut value_counts: BTreeMap<String, u64> = BTreeMap::new();
        for doc_id in candidate_ids {
            if doc_store.get(doc_id)?.is_none() {
                return Err(StorageBackendError::Other(format!(
                    "vector facet candidate {doc_id} is missing from the document store"
                )));
            }
            if let Some(value) = doc_store.get_field(doc_id, &self.facet_field)? {
                if !matches!(value, Value::Null) {
                    let key = value_to_facet_string(&value);
                    let count = value_counts.entry(key).or_insert(0);
                    *count = count.checked_add(1).ok_or_else(|| {
                        StorageBackendError::Other("vector facet count overflowed u64".to_string())
                    })?;
                }
            }
        }
        let mut entries: Vec<PostingEntry> = Vec::with_capacity(value_counts.len());
        for (i, (value, count)) in value_counts.into_iter().enumerate() {
            if count > 9_007_199_254_740_992 {
                return Err(StorageBackendError::Other(format!(
                    "vector facet count {count} cannot be represented exactly as an f64 score"
                )));
            }
            let mut fields = BTreeMap::new();
            fields.insert(
                "_facet_field".to_string(),
                Value::Str(self.facet_field.clone()),
            );
            fields.insert("_facet_value".to_string(), Value::Str(value));
            fields.insert(
                "_facet_count".to_string(),
                Value::Int(i64::try_from(count).map_err(|_| {
                    StorageBackendError::Other(format!(
                        "vector facet count {count} exceeds the Value::Int range"
                    ))
                })?),
            );
            entries.push(PostingEntry::new(
                DocId::try_from(i).map_err(|_| {
                    StorageBackendError::Other(format!(
                        "vector facet bucket index {i} exceeds the document-id range"
                    ))
                })?,
                Payload {
                    positions: Vec::new(),
                    score: count as f64,
                    fields,
                },
            ));
        }
        Ok(PostingList::from_sorted_unchecked(entries))
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
    fn execute(&self, ctx: &ExecutionContext) -> OperatorResult {
        if self.signals.is_empty() {
            return Err(StorageBackendError::Other(
                "adaptive log-odds fusion requires at least one signal".to_string(),
            ));
        }
        if !self.base_alpha.is_finite() || !(0.0..=1.0).contains(&self.base_alpha) {
            return Err(StorageBackendError::Other(format!(
                "adaptive log-odds fusion alpha must be finite and in [0, 1], got {}",
                self.base_alpha
            )));
        }
        let posting_lists: Vec<PostingList> = self
            .signals
            .iter()
            .map(|sig| sig.execute(ctx))
            .collect::<StorageBackendResult<_>>()?;
        for posting_list in &posting_lists {
            validate_probability_postings(posting_list, "adaptive log-odds fusion")?;
        }
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
            return Ok(PostingList::new());
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
        let mut fusion = AdaptiveLogOddsFuser::new(self.base_alpha);
        if let Some(name) = &self.gating {
            let gating = LogitGating::parse(name).ok_or_else(|| {
                StorageBackendError::Other(format!("unknown logit gating function: {name}"))
            })?;
            fusion = fusion.with_gating(gating);
        }
        let mut entries: Vec<PostingEntry> = Vec::with_capacity(num_docs);
        for doc_id in &all_doc_ids {
            let probs: Vec<f64> = score_maps
                .iter()
                .zip(&defaults)
                .map(|(m, def)| m.get(doc_id).copied().unwrap_or(*def))
                .collect();
            let fused = fusion
                .fuse(&probs, &qualities)
                .map_err(|error| StorageBackendError::Other(error.to_string()))?;
            entries.push(PostingEntry::new(*doc_id, Payload::with_score(fused)));
        }
        Ok(PostingList::from_sorted_unchecked(entries))
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
    fn execute(&self, _ctx: &ExecutionContext) -> OperatorResult {
        Ok(self.index.scan(&self.predicate))
    }

    fn cost_estimate(&self, _stats: &IndexStats) -> f64 {
        self.index.scan_cost(&self.predicate)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct LiteralOperator(Vec<(DocId, f64)>);

    impl Operator for LiteralOperator {
        fn execute(&self, _ctx: &ExecutionContext) -> OperatorResult {
            Ok(PostingList::from_sorted_unchecked(
                self.0
                    .iter()
                    .map(|(doc_id, score)| PostingEntry::new(*doc_id, Payload::with_score(*score)))
                    .collect(),
            ))
        }

        fn cost_estimate(&self, _stats: &IndexStats) -> f64 {
            self.0.len() as f64
        }
    }

    #[test]
    fn adaptive_weights_favor_the_discriminating_signal() {
        let fuser = LogOddsFusion::new(0.5).expect("test alpha is valid");
        let flat: BTreeMap<DocId, f64> = [(1, 0.7), (2, 0.7), (3, 0.7)].into_iter().collect();
        let spread: BTreeMap<DocId, f64> = [(1, 0.9), (2, 0.5), (3, 0.1)].into_iter().collect();
        let weights = adaptive_signal_weights(&fuser, &[flat.clone(), spread])
            .expect("spread signal yields weights");
        assert!(
            weights[1] > weights[0],
            "discriminating signal must outweigh the flat one: {weights:?}"
        );
        assert!((weights.iter().sum::<f64>() - 1.0).abs() < 1e-9);
        assert!(
            adaptive_signal_weights(&fuser, &[flat.clone(), flat]).is_none(),
            "all-flat signals fall back to the unweighted mean"
        );
    }

    #[test]
    fn adaptive_operator_wires_gating_to_the_fuser() {
        let signals: Vec<Arc<dyn Operator>> = vec![
            Arc::new(LiteralOperator(vec![(1, 0.2)])),
            Arc::new(LiteralOperator(vec![(1, 0.3)])),
        ];
        let softplus = AdaptiveLogOddsFusionOperator::new(signals.clone(), 0.5, None)
            .execute(&ExecutionContext::new())
            .unwrap();
        let pass = AdaptiveLogOddsFusionOperator::new(signals, 0.5, Some("pass".into()))
            .execute(&ExecutionContext::new())
            .unwrap();
        let softplus_score = softplus.entries()[0].payload.score;
        let pass_score = pass.entries()[0].payload.score;
        assert!(
            softplus_score > 0.5,
            "softplus floors weak evidence, got {softplus_score}"
        );
        assert!(
            pass_score < 0.5,
            "pass gating lets weak evidence sink, got {pass_score}"
        );
    }

    fn two_literal_signals() -> Vec<Arc<dyn Operator>> {
        vec![
            Arc::new(LiteralOperator(vec![(1, 0.8)])),
            Arc::new(LiteralOperator(vec![(1, 0.7)])),
        ]
    }

    #[test]
    fn malformed_log_odds_configuration_returns_operator_error() {
        let context = ExecutionContext::new();
        let cases = [
            LogOddsFusionOperator::new(two_literal_signals(), f64::NAN),
            LogOddsFusionOperator::new(two_literal_signals(), 0.5).with_weights(vec![0.8, 0.8]),
            LogOddsFusionOperator::new(two_literal_signals(), 0.5)
                .with_logit_normalization(vec![0.0, 1.0], vec![0.0, 2.0]),
        ];

        for operator in cases {
            let error = operator
                .execute(&context)
                .expect_err("malformed log-odds configuration must fail");
            assert!(
                error.to_string().contains("log-odds")
                    || error.to_string().contains("weights")
                    || error.to_string().contains("bounds"),
                "unexpected error: {error}"
            );
        }
    }

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
