//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Numerically stable sigmoid / logit and probabilistic AND/OR/NOT in log
//! space. The explicitly named `confidence_scaled_log_odds_pool` implements
//! the robust `n^alpha` ranking heuristic; exact signed single-prior evidence
//! fusion lives in `uqa-fusion::BayesianEvidenceFusion`.

/// Probability clamp epsilon (Eq. 40, Paper 3).
pub const PROB_EPSILON: f64 = 1e-10;

#[inline]
pub fn clamp_prob(p: f64) -> f64 {
    p.clamp(PROB_EPSILON, 1.0 - PROB_EPSILON)
}

/// Numerically stable sigmoid:
/// - `x >= 0`: `1 / (1 + exp(-x))`
/// - `x <  0`: `exp(x) / (1 + exp(x))`
#[inline]
pub fn sigmoid(x: f64) -> f64 {
    if x >= 0.0 {
        1.0 / (1.0 + (-x).exp())
    } else {
        let e = x.exp();
        e / (1.0 + e)
    }
}

/// Inverse sigmoid: `log(p / (1 - p))`. Input is clamped to
/// `(epsilon, 1 - epsilon)` first.
#[inline]
pub fn logit(p: f64) -> f64 {
    let p = clamp_prob(p);
    (p / (1.0 - p)).ln()
}

/// Cosine similarity to probability: `(1 + score) / 2` clamped to
/// `(epsilon, 1 - epsilon)` (Definition 7.1.2, Paper 3).
#[inline]
pub fn cosine_to_probability(score: f64) -> f64 {
    clamp_prob(f64::midpoint(1.0, score))
}

/// Probabilistic NOT: `1 - p`.
#[inline]
pub fn prob_not(p: f64) -> f64 {
    clamp_prob(1.0 - clamp_prob(p))
}

/// Probabilistic AND in log space: `exp(sum(ln p_i))`.
pub fn prob_and(probs: &[f64]) -> f64 {
    if probs.is_empty() {
        return 1.0;
    }
    let s: f64 = probs.iter().map(|&p| clamp_prob(p).ln()).sum();
    s.exp()
}

/// Probabilistic OR in log space: `1 - exp(sum(ln(1 - p_i)))`.
pub fn prob_or(probs: &[f64]) -> f64 {
    if probs.is_empty() {
        return 0.0;
    }
    let s: f64 = probs.iter().map(|&p| (1.0 - clamp_prob(p)).ln()).sum();
    1.0 - s.exp()
}

/// Confidence-scaled log-odds ranking pool.
///
/// `P_final = sigmoid((1 / n^(1-alpha)) * sum(logit(p_i)))`
///
/// Rearranged in implementation form: `sigmoid(n^alpha * mean(logit p_i))`.
/// Default `alpha = 0.5` yields the `sqrt(n)` law. This confidence scaling is
/// a ranking heuristic, not the exact single-prior Bayesian evidence theorem.
pub fn confidence_scaled_log_odds_pool(probs: &[f64], alpha: f64) -> f64 {
    if probs.is_empty() {
        return 0.5;
    }
    let n = probs.len() as f64;
    let mean_logit: f64 = probs.iter().map(|&p| logit(p)).sum::<f64>() / n;
    sigmoid(mean_logit * n.powf(alpha))
}

/// Weighted confidence-scaled log-odds ranking pool.
///
/// `sigmoid(n^alpha * sum(w_i * logit(p_i)))`
///
/// `weights` must be non-negative and sum to ~1. Returns `Err` otherwise.
pub fn confidence_scaled_log_odds_pool_weighted(
    probs: &[f64],
    weights: &[f64],
    alpha: f64,
) -> Result<f64, &'static str> {
    if probs.len() != weights.len() {
        return Err("probs and weights must have the same length");
    }
    if probs.is_empty() {
        return Ok(0.5);
    }
    if weights.iter().any(|w| *w < 0.0) {
        return Err("weights must be non-negative");
    }
    let sum_w: f64 = weights.iter().sum();
    if (sum_w - 1.0).abs() > 1e-6 {
        return Err("weights must sum to 1");
    }
    let n = probs.len() as f64;
    let weighted_logit: f64 = probs.iter().zip(weights).map(|(&p, &w)| w * logit(p)).sum();
    Ok(sigmoid(n.powf(alpha) * weighted_logit))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx_eq(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-9, "expected {a} ~ {b}");
    }

    #[test]
    fn sigmoid_logit_round_trip() {
        for p in [0.01, 0.1, 0.3, 0.5, 0.7, 0.9, 0.99] {
            approx_eq(sigmoid(logit(p)), p);
        }
    }

    #[test]
    fn sigmoid_handles_extremes() {
        assert!(sigmoid(50.0) > 1.0 - 1e-10);
        assert!(sigmoid(-50.0) < 1e-10);
        assert!(sigmoid(0.0) - 0.5 < 1e-12);
    }

    #[test]
    fn cosine_maps_to_unit_interval() {
        approx_eq(cosine_to_probability(1.0), 1.0 - PROB_EPSILON);
        approx_eq(cosine_to_probability(-1.0), PROB_EPSILON);
        approx_eq(cosine_to_probability(0.0), 0.5);
    }

    #[test]
    fn prob_and_log_space_matches_product() {
        approx_eq(prob_and(&[0.5, 0.5, 0.5]), 0.125);
        approx_eq(prob_and(&[0.9, 0.8]), 0.72);
    }

    #[test]
    fn prob_or_log_space_matches_inclusion_exclusion() {
        approx_eq(prob_or(&[0.5, 0.5]), 0.75);
        approx_eq(prob_or(&[0.0, 0.0]), 0.0);
    }

    #[test]
    fn confidence_scaled_log_odds_pool_n1_identity() {
        approx_eq(confidence_scaled_log_odds_pool(&[0.7], 0.5), 0.7);
    }

    #[test]
    fn confidence_scaled_log_odds_pool_scale_neutral_at_alpha_zero() {
        // alpha = 0 is the only setting that gives scale neutrality
        // (P_final = p when all P_i = p). The default alpha = 0.5
        // intentionally amplifies agreement away from the mean.
        for p in [0.2, 0.5, 0.8] {
            for n in 1..6 {
                let probs = vec![p; n];
                let got = confidence_scaled_log_odds_pool(&probs, 0.0);
                approx_eq(got, p);
            }
        }
    }

    #[test]
    fn confidence_scaled_log_odds_pool_amplifies_agreement_at_alpha_half() {
        // With alpha = 0.5 and all-equal P_i > 0.5, P_final pushes the
        // probability further away from 0.5 as n grows (Theorem 4.3.x in
        // Paper 4: agreement amplification). Symmetric on the
        // irrelevance side: P_i < 0.5 -> P_final < P_i.
        let p = 0.7;
        let p1 = confidence_scaled_log_odds_pool(&[p], 0.5);
        let p3 = confidence_scaled_log_odds_pool(&[p; 3], 0.5);
        let p5 = confidence_scaled_log_odds_pool(&[p; 5], 0.5);
        assert!(p1 < p3 && p3 < p5, "amplification: {p1} < {p3} < {p5}");
    }

    #[test]
    fn confidence_scaled_log_odds_pool_irrelevance_preserving() {
        // All P_i < 0.5 implies P_final < 0.5.
        let probs = [0.2, 0.3, 0.4];
        let got = confidence_scaled_log_odds_pool(&probs, 0.5);
        assert!(got < 0.5, "got {got}");
    }

    #[test]
    fn confidence_scaled_log_odds_pool_relevance_preserving() {
        let probs = [0.6, 0.7, 0.8];
        let got = confidence_scaled_log_odds_pool(&probs, 0.5);
        assert!(got > 0.5, "got {got}");
    }

    #[test]
    fn confidence_scaled_log_odds_pool_symmetric_disagreement_collapses_to_half() {
        // logit(0.3) and logit(0.7) cancel.
        let got = confidence_scaled_log_odds_pool(&[0.3, 0.7], 0.5);
        approx_eq(got, 0.5);
    }

    #[test]
    fn weighted_log_odds_rejects_bad_weights() {
        assert!(confidence_scaled_log_odds_pool_weighted(&[0.5, 0.5], &[0.5, 0.6], 0.0).is_err());
        assert!(confidence_scaled_log_odds_pool_weighted(&[0.5, 0.5], &[-0.1, 1.1], 0.0).is_err());
    }
}
