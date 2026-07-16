//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Coverage for `test_scoring`.

use std::sync::Arc;

use uqa_core::IndexStats;
use uqa_scoring::{
    BM25Params, BM25Scorer, BayesianBM25Params, BayesianBM25Scorer, BayesianProbabilityTransform,
    VectorScorer,
};

fn stats() -> Arc<IndexStats> {
    let mut stats = IndexStats::default();
    stats.total_docs = 10_000;
    stats.avg_doc_length = 200.0;
    Arc::new(stats)
}

fn bm25_scorer() -> BM25Scorer {
    BM25Scorer::new(BM25Params::default(), stats())
}

fn bayesian_scorer() -> BayesianBM25Scorer {
    BayesianBM25Scorer::new(BayesianBM25Params::default(), stats())
}

fn assert_close(a: f64, b: f64, eps: f64) {
    assert!((a - b).abs() <= eps, "{a} != {b} within {eps}");
}

#[test]
fn test_idf_positive_for_rare_terms() {
    assert!(bm25_scorer().idf(10) > 0.0);
}

#[test]
fn test_idf_decreases_with_frequency() {
    let scorer = bm25_scorer();
    assert!(scorer.idf(10) > scorer.idf(5000));
}

#[test]
fn test_monotonicity_term_frequency() {
    let scorer = bm25_scorer();
    let low = scorer.score(1, 200, 100);
    let mid = scorer.score(5, 200, 100);
    let high = scorer.score(20, 200, 100);
    assert!(low < mid && mid < high);
}

#[test]
fn test_monotonicity_doc_length() {
    let scorer = bm25_scorer();
    let short = scorer.score(5, 50, 100);
    let avg = scorer.score(5, 200, 100);
    let long = scorer.score(5, 500, 100);
    assert!(short > avg && avg > long);
}

#[test]
fn test_upper_bound() {
    let scorer = bm25_scorer();
    let upper_bound = scorer.upper_bound(100);
    for tf in [1, 5, 10, 50, 100, 1000] {
        for dl in [1, 50, 100, 200, 500, 1000] {
            let score = scorer.score(tf, dl, 100);
            assert!(
                score < upper_bound,
                "score={score} ub={upper_bound} tf={tf} dl={dl}"
            );
        }
    }
}

#[test]
fn test_score_non_negative() {
    assert!(bm25_scorer().score(1, 200, 100) >= 0.0);
}

#[test]
fn test_boosted_upper_bound() {
    let scorer = BM25Scorer::new(
        BM25Params {
            boost: 2.5,
            ..BM25Params::default()
        },
        stats(),
    );
    let upper_bound = scorer.upper_bound(50);
    assert!(scorer.score(100, 10, 50) < upper_bound);
}

#[test]
fn test_output_in_unit_interval() {
    let scorer = bayesian_scorer();
    for tf in [0, 1, 5, 10, 50] {
        for dl in [10, 100, 200, 500, 1000] {
            for df in [1, 10, 100, 1000, 5000] {
                let p = scorer.score(tf, dl, df);
                assert!((0.0..=1.0).contains(&p), "p={p} tf={tf} dl={dl} df={df}");
            }
        }
    }
}

#[test]
fn test_monotonicity_higher_bm25_higher_posterior() {
    let scorer = bayesian_scorer();
    let low = scorer.score(1, 200, 100);
    let mid = scorer.score(5, 200, 100);
    let high = scorer.score(20, 200, 100);
    assert!(low < mid && mid < high);
}

#[test]
fn test_composite_prior_bounds() {
    for tf in [0.0, 1.0, 5.0, 10.0, 50.0, 100.0] {
        for ratio in [0.005, 0.05, 0.5, 1.0, 2.5, 10.0] {
            let prior = BayesianProbabilityTransform::composite_prior(tf, ratio);
            assert!(
                (0.1..=0.9).contains(&prior),
                "prior={prior} tf={tf} ratio={ratio}"
            );
        }
    }
}

#[test]
fn test_base_rate_identity() {
    let with_base_rate = BayesianBM25Scorer::new(
        BayesianBM25Params {
            base_rate: 0.5,
            ..BayesianBM25Params::default()
        },
        stats(),
    );
    let default = bayesian_scorer();
    for tf in [1, 5, 10] {
        for dl in [100, 200, 500] {
            assert_close(
                with_base_rate.score(tf, dl, 100),
                default.score(tf, dl, 100),
                1e-12,
            );
        }
    }
}

#[test]
fn test_base_rate_never_shifts_the_posterior() {
    let posterior_for = |base_rate: f64| {
        BayesianBM25Scorer::new(
            BayesianBM25Params {
                base_rate,
                ..BayesianBM25Params::default()
            },
            stats(),
        )
        .score(5, 200, 100)
    };
    let default = posterior_for(0.0);
    assert_close(posterior_for(0.2), default, 1e-12);
    assert_close(posterior_for(0.8), default, 1e-12);
}

#[test]
fn test_base_rate_shifts_the_evidence() {
    let evidence_for = |base_rate: f64| {
        let params = BayesianBM25Params {
            base_rate,
            ..BayesianBM25Params::default()
        };
        BayesianBM25Scorer::new(params.evidence_params(), stats()).score(5, 200, 100)
    };
    // A smaller corpus prior makes any match stronger evidence.
    let low_prior = evidence_for(0.2);
    let neutral = evidence_for(0.5);
    let high_prior = evidence_for(0.8);
    assert!(low_prior > neutral && neutral > high_prior);
}

#[test]
fn test_upper_bound_at_least_score() {
    let scorer = bayesian_scorer();
    let upper_bound = scorer.upper_bound(100);
    for tf in [1, 5, 10, 50] {
        for dl in [50, 200, 500] {
            assert!(scorer.score(tf, dl, 100) <= upper_bound + 1e-10);
        }
    }
}

#[test]
fn test_likelihood_numerically_stable() {
    let transform = BayesianProbabilityTransform::new(1.0, 0.0, None);
    assert_close(transform.likelihood(500.0), 1.0, 1e-12);
    assert_close(transform.likelihood(-500.0), 0.0, 1e-12);
    assert_close(transform.likelihood(0.0), 0.5, 1e-12);
}

#[test]
fn test_cosine_similarity_identical() {
    let v = [1.0, 2.0, 3.0];
    assert_close(VectorScorer::cosine_similarity(&v, &v), 1.0, 1e-9);
}

#[test]
fn test_cosine_similarity_opposite() {
    let v = [1.0, 0.0, 0.0];
    let neg = [-1.0, -0.0, -0.0];
    assert_close(VectorScorer::cosine_similarity(&v, &neg), -1.0, 1e-9);
}

#[test]
fn test_cosine_similarity_orthogonal() {
    let a = [1.0, 0.0];
    let b = [0.0, 1.0];
    assert_close(VectorScorer::cosine_similarity(&a, &b), 0.0, 1e-9);
}

#[test]
fn test_cosine_similarity_zero_vector() {
    let a = [1.0, 2.0, 3.0];
    let zero = [0.0, 0.0, 0.0];
    assert_eq!(VectorScorer::cosine_similarity(&a, &zero), 0.0);
    assert_eq!(VectorScorer::cosine_similarity(&zero, &zero), 0.0);
}

#[test]
fn test_similarity_to_probability_range() {
    assert_close(VectorScorer::similarity_to_probability(1.0), 1.0, 1e-9);
    assert_close(VectorScorer::similarity_to_probability(-1.0), 0.0, 1e-9);
    assert_close(VectorScorer::similarity_to_probability(0.0), 0.5, 1e-9);
}

#[test]
fn test_similarity_to_probability_monotonic() {
    let sims = [-0.8, -0.3, 0.0, 0.4, 0.9];
    let probs: Vec<f64> = sims
        .iter()
        .map(|sim| VectorScorer::similarity_to_probability(*sim))
        .collect();
    assert!(probs.windows(2).all(|w| w[0] < w[1]));
}
