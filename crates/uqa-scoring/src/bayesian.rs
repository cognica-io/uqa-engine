//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bayesian probability transform: sigmoid likelihood + composite prior
//! (Eq. 25-27, Paper 3) + two-step posterior (Remark 4.4.5).

use crate::prob::{clamp_prob, sigmoid};
use crate::{error::invalid_input, ScoringResult};

#[derive(Debug, Clone, Copy)]
pub struct BayesianProbabilityTransform {
    pub alpha: f64,
    pub beta: f64,
    /// Optional corpus-level base rate. `None` is equivalent to `0.5`
    /// (logit = 0, i.e. no base-rate correction).
    pub base_rate: Option<f64>,
}

impl Default for BayesianProbabilityTransform {
    fn default() -> Self {
        Self {
            alpha: 1.0,
            beta: 0.0,
            base_rate: None,
        }
    }
}

impl BayesianProbabilityTransform {
    pub fn new(alpha: f64, beta: f64, base_rate: Option<f64>) -> ScoringResult<Self> {
        if !alpha.is_finite() || !beta.is_finite() {
            return Err(invalid_input(format!(
                "Bayesian alpha and beta must be finite, got alpha={alpha}, beta={beta}"
            )));
        }
        if let Some(br) = base_rate {
            if !br.is_finite() || !(0.0..1.0).contains(&br) || br == 0.0 {
                return Err(invalid_input(format!(
                    "base_rate must be finite and in (0, 1), got {br}"
                )));
            }
        }
        Ok(Self {
            alpha,
            beta,
            base_rate,
        })
    }

    /// Sigmoid likelihood: `sigma(alpha * (score - beta))` (Eq. 20).
    #[inline]
    pub fn likelihood(&self, score: f64) -> f64 {
        sigmoid(self.alpha * (score - self.beta))
    }

    /// Term-frequency prior (Eq. 25):
    /// `P_tf(tf) = 0.2 + 0.7 * min(1, tf / 10)`.
    #[inline]
    pub fn tf_prior(tf: f64) -> f64 {
        0.2 + 0.7 * (tf / 10.0).min(1.0)
    }

    /// Document-length normalisation prior (Eq. 26):
    /// `P_norm(r) = 0.3 + 0.6 * (1 - min(1, |r - 0.5| * 2))`,
    /// peaks at 0.9 when `r = 0.5`, floor 0.3 outside `[0, 1]`.
    #[inline]
    pub fn norm_prior(doc_len_ratio: f64) -> f64 {
        0.3 + 0.6 * (1.0 - ((doc_len_ratio - 0.5).abs() * 2.0).min(1.0))
    }

    /// Composite prior (Eq. 27):
    /// `clamp(0.7 * P_tf + 0.3 * P_norm, 0.1, 0.9)`.
    #[inline]
    pub fn composite_prior(tf: f64, doc_len_ratio: f64) -> f64 {
        let p_tf = Self::tf_prior(tf);
        let p_norm = Self::norm_prior(doc_len_ratio);
        (0.7 * p_tf + 0.3 * p_norm).clamp(0.1, 0.9)
    }

    /// Posterior floor for a field with no match evidence: zero term
    /// frequency and the length-normalisation floor (Eq. 25-27 with
    /// `tf = 0` and an average-length document). Multi-field fusion
    /// pads absent fields with this value so that matching a field can
    /// never rank a document below one that did not match it: with
    /// `beta <= 0` a BM25 score of at least zero keeps the likelihood
    /// at or above 0.5, so every matched posterior stays at or above
    /// its composite prior, which in turn exceeds this floor.
    pub fn no_match_prior() -> f64 {
        Self::composite_prior(0.0, 1.0)
    }

    /// Bayesian posterior via two-step update (Eq. 22, Remark 4.4.5).
    ///
    /// Without `base_rate`:
    /// `P = L*p / (L*p + (1-L)*(1-p))`.
    ///
    /// With `base_rate` (the second update is equivalent to adding
    /// `logit(base_rate)` in log-odds space):
    /// `Step 1: p1 = L*p / (L*p + (1-L)*(1-p))`
    /// `Step 2: P  = p1*br / (p1*br + (1-p1)*(1-br))`.
    pub fn posterior(likelihood: f64, prior: f64, base_rate: Option<f64>) -> f64 {
        let l = likelihood;
        let p = prior;
        let num = l * p;
        let denom = num + (1.0 - l) * (1.0 - p);
        let mut result = clamp_prob(num / denom);
        if let Some(br) = base_rate {
            let n2 = result * br;
            let d2 = n2 + (1.0 - result) * (1.0 - br);
            result = clamp_prob(n2 / d2);
        }
        result
    }

    /// BM25 score -> calibrated probability via likelihood, composite
    /// prior, and posterior. `tf` is term frequency, `doc_len_ratio`
    /// is `doc_length / avg_doc_length`.
    pub fn score_to_probability(&self, score: f64, tf: f64, doc_len_ratio: f64) -> f64 {
        let l = self.likelihood(score);
        let prior = Self::composite_prior(tf, doc_len_ratio);
        Self::posterior(l, prior, self.base_rate)
    }

    /// Bayesian WAND upper bound (Theorem 6.1.2, Paper 3): given a BM25
    /// upper bound, compute the maximum possible Bayesian posterior by
    /// pretending the prior took its highest possible value.
    pub fn wand_upper_bound(&self, bm25_upper_bound: f64, p_max: f64) -> f64 {
        let l_max = self.likelihood(bm25_upper_bound);
        Self::posterior(l_max, p_max, self.base_rate)
    }
}

#[cfg(test)]
mod no_match_prior_tests {
    use super::*;

    #[test]
    fn no_match_prior_matches_zero_evidence_composite_prior() {
        let floor = BayesianProbabilityTransform::no_match_prior();
        assert!((floor - 0.23).abs() < 1e-12, "{floor}");
    }

    #[test]
    fn matched_posteriors_stay_above_the_no_match_prior_under_defaults() {
        let transform = BayesianProbabilityTransform::default();
        let floor = BayesianProbabilityTransform::no_match_prior();
        for score_tenths in 0..=100 {
            let score = f64::from(score_tenths) / 10.0;
            for tf in 1..=10 {
                for ratio_tenths in 0..=30 {
                    let ratio = f64::from(ratio_tenths) / 10.0;
                    let posterior = transform.score_to_probability(score, f64::from(tf), ratio);
                    assert!(
                        posterior >= floor,
                        "posterior {posterior} fell below floor {floor} \
                         at score={score} tf={tf} ratio={ratio}",
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prob::{logit, sigmoid};

    fn approx_eq(a: f64, b: f64, eps: f64) {
        assert!((a - b).abs() < eps, "expected {a} ~ {b} within {eps}");
    }

    #[test]
    fn likelihood_at_beta_is_half() {
        let t = BayesianProbabilityTransform::new(1.0, 5.0, None).unwrap();
        approx_eq(t.likelihood(5.0), 0.5, 1e-12);
    }

    #[test]
    fn constructor_rejects_non_finite_parameters_and_invalid_base_rates() {
        assert!(BayesianProbabilityTransform::new(f64::NAN, 0.0, None).is_err());
        assert!(BayesianProbabilityTransform::new(1.0, f64::INFINITY, None).is_err());
        assert!(BayesianProbabilityTransform::new(1.0, 0.0, Some(0.0)).is_err());
        assert!(BayesianProbabilityTransform::new(1.0, 0.0, Some(1.0)).is_err());
        assert!(BayesianProbabilityTransform::new(1.0, 0.0, Some(f64::NAN)).is_err());
    }

    #[test]
    fn tf_prior_floor_and_ceiling() {
        approx_eq(BayesianProbabilityTransform::tf_prior(0.0), 0.2, 1e-12);
        approx_eq(BayesianProbabilityTransform::tf_prior(10.0), 0.9, 1e-12);
        approx_eq(BayesianProbabilityTransform::tf_prior(100.0), 0.9, 1e-12);
    }

    #[test]
    fn norm_prior_peaks_at_half() {
        approx_eq(BayesianProbabilityTransform::norm_prior(0.5), 0.9, 1e-12);
        approx_eq(BayesianProbabilityTransform::norm_prior(0.0), 0.3, 1e-12);
        approx_eq(BayesianProbabilityTransform::norm_prior(1.0), 0.3, 1e-12);
        approx_eq(BayesianProbabilityTransform::norm_prior(2.0), 0.3, 1e-12);
    }

    #[test]
    fn composite_prior_in_clamp_window() {
        for tf in [0.0, 1.0, 5.0, 50.0] {
            for r in [0.0, 0.5, 1.0, 2.0] {
                let p = BayesianProbabilityTransform::composite_prior(tf, r);
                assert!((0.1..=0.9).contains(&p), "p={p} for tf={tf} r={r}");
            }
        }
    }

    #[test]
    fn posterior_two_step_matches_logit_form() {
        // sigmoid(logit(L) + logit(p) + logit(br)) should equal posterior(L, p, br).
        let l = 0.7;
        let p = 0.4;
        let br = 0.3;

        let two_step = BayesianProbabilityTransform::posterior(l, p, Some(br));
        let logit_form = sigmoid(logit(l) + logit(p) + logit(br));
        approx_eq(two_step, logit_form, 1e-9);
    }

    #[test]
    fn posterior_no_base_rate_matches_logit_form() {
        let l = 0.6;
        let p = 0.3;
        let two_step = BayesianProbabilityTransform::posterior(l, p, None);
        let logit_form = sigmoid(logit(l) + logit(p));
        approx_eq(two_step, logit_form, 1e-9);
    }

    #[test]
    fn posterior_monotone_in_likelihood() {
        let p = 0.5;
        let prev = BayesianProbabilityTransform::posterior(0.1, p, None);
        let mut last = prev;
        for l in [0.2, 0.3, 0.5, 0.7, 0.9] {
            let cur = BayesianProbabilityTransform::posterior(l, p, None);
            assert!(cur > last, "posterior should rise with L: {last} -> {cur}");
            last = cur;
        }
    }

    #[test]
    fn score_to_probability_pipeline() {
        let t = BayesianProbabilityTransform::new(1.0, 0.0, None).unwrap();
        // For score=0 the likelihood is 0.5, posterior collapses to the prior.
        let p_zero = t.score_to_probability(0.0, 0.0, 1.0);
        approx_eq(
            p_zero,
            BayesianProbabilityTransform::composite_prior(0.0, 1.0),
            1e-12,
        );
    }

    #[test]
    fn wand_upper_bound_dominates_actual_posterior() {
        let t = BayesianProbabilityTransform::new(1.0, 0.0, None).unwrap();
        let upper = t.wand_upper_bound(5.0, 0.9);
        // Any actual posterior with score <= 5.0 must not exceed `upper`.
        for tf in [0.0, 1.0, 10.0] {
            for r in [0.1, 0.5, 1.0, 2.0] {
                let actual = t.score_to_probability(5.0, tf, r);
                assert!(actual <= upper + 1e-12, "actual {actual} > upper {upper}");
            }
        }
    }
}
