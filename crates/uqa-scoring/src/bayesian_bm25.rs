//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Query-level Bayesian calibration for BM25 scores.

use std::sync::Arc;

use uqa_core::IndexStats;

use crate::bm25::{BM25Params, BM25Scorer};
use crate::prob::{logit, sigmoid};

/// Floor on the scaled score spread, as a fraction of the reference
/// spread, so extrapolating to very short queries cannot drive `sigma`
/// to zero and explode `alpha` into a step function.
const MIN_SIGMA_SCALE: f64 = 0.25;

#[derive(Debug, Clone, Copy)]
pub struct BayesianBM25Params {
    pub bm25: BM25Params,
    pub alpha: f64,
    pub beta: f64,
    /// Corpus relevance prior in `[0, 1)`; zero means "not estimated".
    /// The prior never enters the posterior transform (which matches
    /// Lucene's `BayesianScoreQuery` exactly); it is metadata for
    /// fusion, where it converts posteriors to evidence and enters the
    /// fused score exactly once.
    pub base_rate: f64,
    /// Query length (analyzed term count) the calibration was fitted
    /// at. Zero disables query-length scaling, so hand-written and
    /// learner-fitted parameters apply verbatim.
    pub calibration_tokens: f64,
    /// Fitted per-token slope of the sigmoid midpoint: raw BM25 sums
    /// grow with the number of query terms, and `beta` must track that
    /// scale for the posterior to stay in its linear region.
    pub beta_slope: f64,
    /// Fitted per-token slope of the score spread (`1 / alpha`).
    pub sigma_slope: f64,
}

impl BayesianBM25Params {
    /// Parameters whose posterior equals this calibration's prior-free
    /// evidence `sigmoid(alpha * (raw - beta) - logit(base_rate))`,
    /// expressed through the equivalent midpoint shift
    /// `beta + logit(base_rate) / alpha`. With no estimated prior the
    /// calibration is returned unchanged.
    pub fn evidence_params(&self) -> Self {
        if self.base_rate <= 0.0 {
            return *self;
        }
        Self {
            beta: self.beta + logit(self.base_rate) / self.alpha,
            base_rate: 0.0,
            ..*self
        }
    }

    /// The calibration translated to a query with `term_count` analyzed
    /// terms. Raw BM25 query scores are sums over query terms, so both
    /// the midpoint and the spread of the matching-score distribution
    /// move with the term count; the estimator fits those slopes and
    /// this method applies them:
    ///
    /// `beta_q = beta + beta_slope * (q - q_ref)`
    /// `sigma_q = max(sigma_ref + sigma_slope * (q - q_ref), floor)`
    ///
    /// Parameters without a fitted reference length (or a zero term
    /// count) are returned unchanged. Scaling happens per query, never
    /// per document, so within-query ranking stays monotone in the raw
    /// score.
    pub fn scaled_for_query_terms(&self, term_count: usize) -> Self {
        if self.calibration_tokens <= 0.0 || term_count == 0 {
            return *self;
        }
        let delta = term_count as f64 - self.calibration_tokens;
        if delta == 0.0 {
            return *self;
        }
        let sigma_reference = self.alpha.recip();
        let sigma =
            (sigma_reference + self.sigma_slope * delta).max(sigma_reference * MIN_SIGMA_SCALE);
        Self {
            beta: self.beta + self.beta_slope * delta,
            alpha: sigma.recip(),
            ..*self
        }
    }
}

impl Default for BayesianBM25Params {
    fn default() -> Self {
        Self {
            bm25: BM25Params::default(),
            alpha: 1.0,
            beta: 0.0,
            base_rate: 0.0,
            calibration_tokens: 0.0,
            beta_slope: 0.0,
            sigma_slope: 0.0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct BayesianBM25Scorer {
    pub params: BayesianBM25Params,
    pub bm25: BM25Scorer,
}

impl BayesianBM25Scorer {
    pub fn new(params: BayesianBM25Params, stats: Arc<IndexStats>) -> Self {
        assert!(
            params.alpha.is_finite() && params.alpha > 0.0,
            "alpha must be a positive finite value, got {}",
            params.alpha
        );
        assert!(
            params.beta.is_finite(),
            "beta must be a finite value, got {}",
            params.beta
        );
        assert!(
            params.base_rate.is_finite() && params.base_rate >= 0.0 && params.base_rate < 1.0,
            "base_rate must be in [0, 1), got {}",
            params.base_rate
        );
        Self {
            params,
            bm25: BM25Scorer::new(params.bm25, stats),
        }
    }

    pub fn idf(&self, doc_freq: u64) -> f64 {
        self.bm25.idf(doc_freq)
    }

    /// Score a one-term query and calibrate the complete BM25 score once.
    pub fn score(&self, term_freq: u64, doc_length: u64, doc_freq: u64) -> f64 {
        let idf_val = self.bm25.idf(doc_freq);
        self.score_with_idf(term_freq, doc_length, idf_val)
    }

    pub fn score_with_idf(&self, term_freq: u64, doc_length: u64, idf_val: f64) -> f64 {
        let raw = self.bm25.score_with_idf(term_freq, doc_length, idf_val);
        self.calibrate_raw_score(raw)
    }

    /// Calibrate the complete raw BM25 query score.
    ///
    /// `P = sigmoid(alpha * (score - beta))` -- exactly Lucene's
    /// `BayesianScoreQuery` transform. With the estimator's boundary
    /// anchoring, `beta` sits at the relevance boundary, so the
    /// posterior crosses 0.5 where matches start counting as relevant.
    /// The corpus prior (`params.base_rate`) belongs to fusion, not to
    /// this transform.
    pub fn calibrate_raw_score(&self, raw_score: f64) -> f64 {
        sigmoid(self.params.alpha * (raw_score - self.params.beta))
    }

    /// Combine raw BM25 term contributions, then calibrate exactly once.
    pub fn combine_scores(&self, raw_term_scores: &[f64]) -> f64 {
        self.calibrate_raw_score(BM25Scorer::combine_scores(raw_term_scores))
    }

    /// Bayesian WAND upper bound (Theorem 6.1.2): tightest safe pruning
    /// bound derived from the BM25 supremum and the maximum prior.
    pub fn upper_bound(&self, doc_freq: u64) -> f64 {
        let bm25_ub = self.bm25.upper_bound(doc_freq);
        self.calibrate_raw_score(bm25_ub)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stats(n: u64, avgdl: f64) -> Arc<IndexStats> {
        let mut s = IndexStats::default();
        s.total_docs = n;
        s.avg_doc_length = avgdl;
        Arc::new(s)
    }

    #[test]
    fn score_in_unit_interval() {
        let s = stats(1000, 10.0);
        let scorer = BayesianBM25Scorer::new(BayesianBM25Params::default(), s.clone());
        let p = scorer.score(3, 10, 50);
        assert!(p > 0.0 && p < 1.0, "got {p}");
    }

    #[test]
    fn score_monotone_in_tf() {
        let s = stats(1000, 10.0);
        let scorer = BayesianBM25Scorer::new(BayesianBM25Params::default(), s.clone());
        let mut last = scorer.score(0, 10, 50);
        for tf in 1..20 {
            let cur = scorer.score(tf, 10, 50);
            assert!(cur > last, "tf {tf}: {last} -> {cur}");
            last = cur;
        }
    }

    #[test]
    fn upper_bound_dominates_observed_scores() {
        let s = stats(1000, 10.0);
        let scorer = BayesianBM25Scorer::new(BayesianBM25Params::default(), s.clone());
        let ub = scorer.upper_bound(50);
        for tf in [1, 5, 10, 100] {
            for dl in [1, 5, 50, 500] {
                let p = scorer.score(tf, dl, 50);
                assert!(p <= ub + 1e-12, "tf={tf} dl={dl}: {p} > {ub}");
            }
        }
    }

    #[test]
    fn query_level_calibration_uses_the_bm25_sum() {
        let scorer = BayesianBM25Scorer::new(BayesianBM25Params::default(), stats(1000, 10.0));
        let combined = scorer.combine_scores(&[0.7, 0.4]);
        assert!((combined - sigmoid(1.1)).abs() < 1e-12);
    }

    #[test]
    fn query_level_calibration_preserves_raw_ranking() {
        let scorer = BayesianBM25Scorer::new(BayesianBM25Params::default(), stats(1000, 10.0));
        let lower = scorer.combine_scores(&[0.7, 0.4]);
        let higher = scorer.combine_scores(&[0.8, 0.5]);
        assert!(higher > lower, "{higher} must exceed {lower}");
    }

    #[test]
    fn base_rate_never_enters_the_posterior() {
        let with_prior = BayesianBM25Scorer::new(
            BayesianBM25Params {
                base_rate: 0.1,
                ..BayesianBM25Params::default()
            },
            stats(1000, 10.0),
        );
        let without_prior =
            BayesianBM25Scorer::new(BayesianBM25Params::default(), stats(1000, 10.0));
        let combined = with_prior.combine_scores(&[0.7, 0.4]);
        assert!((combined - without_prior.combine_scores(&[0.7, 0.4])).abs() < 1e-12);
        assert!((combined - sigmoid(1.1)).abs() < 1e-12);
    }

    #[test]
    fn query_length_scaling_translates_the_calibration() {
        let params = BayesianBM25Params {
            alpha: 0.5,
            beta: 6.0,
            calibration_tokens: 5.0,
            beta_slope: 1.2,
            sigma_slope: 0.3,
            ..BayesianBM25Params::default()
        };
        let scaled = params.scaled_for_query_terms(15);
        assert!((scaled.beta - (6.0 + 1.2 * 10.0)).abs() < 1e-12);
        assert!((scaled.alpha - (2.0_f64 + 0.3 * 10.0).recip()).abs() < 1e-12);
        // Slopes and reference travel unchanged.
        assert!((scaled.calibration_tokens - 5.0).abs() < 1e-12);
        assert!((scaled.beta_slope - 1.2).abs() < 1e-12);
    }

    #[test]
    fn query_length_scaling_is_inert_without_a_reference() {
        let params = BayesianBM25Params {
            alpha: 1.7,
            beta: 0.8,
            base_rate: 0.08,
            ..BayesianBM25Params::default()
        };
        let scaled = params.scaled_for_query_terms(12);
        assert!((scaled.alpha - params.alpha).abs() < 1e-12);
        assert!((scaled.beta - params.beta).abs() < 1e-12);
        let reference = BayesianBM25Params {
            calibration_tokens: 5.0,
            ..params
        };
        let same_length = reference.scaled_for_query_terms(5);
        assert!((same_length.beta - reference.beta).abs() < 1e-12);
    }

    #[test]
    fn query_length_scaling_floors_the_spread() {
        let params = BayesianBM25Params {
            alpha: 1.0,
            beta: 6.0,
            calibration_tokens: 5.0,
            beta_slope: 1.2,
            sigma_slope: 0.3,
            ..BayesianBM25Params::default()
        };
        // Extrapolating down to one term would drive sigma negative;
        // the floor keeps alpha finite and bounded.
        let scaled = params.scaled_for_query_terms(1);
        assert!((scaled.alpha - 4.0).abs() < 1e-12, "got {}", scaled.alpha);
    }

    #[test]
    fn evidence_params_subtract_the_prior_in_logit_space() {
        let params = BayesianBM25Params {
            alpha: 2.0,
            beta: 3.0,
            base_rate: 0.05,
            ..BayesianBM25Params::default()
        };
        let posterior_scorer = BayesianBM25Scorer::new(params, stats(1000, 10.0));
        let evidence_scorer = BayesianBM25Scorer::new(params.evidence_params(), stats(1000, 10.0));
        for raw in [0.0, 1.5, 3.0, 6.0] {
            let posterior = posterior_scorer.calibrate_raw_score(raw);
            let evidence = evidence_scorer.calibrate_raw_score(raw);
            let expected = sigmoid(logit(posterior) - logit(0.05));
            assert!(
                (evidence - expected).abs() < 1e-12,
                "raw {raw}: {evidence} vs {expected}"
            );
        }
        let plain = BayesianBM25Params::default();
        assert!((plain.evidence_params().beta - plain.beta).abs() < 1e-12);
    }
}
