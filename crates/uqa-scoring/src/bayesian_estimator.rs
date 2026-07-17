//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Corpus-driven parameter estimation for query-level Bayesian BM25.
//!
//! The sampling loop mirrors Lucene's `BayesianScoreEstimator`: it
//! reservoir samples a field's indexed vocabulary, builds OR
//! pseudo-queries, and gathers their raw BM25 score distributions.
//! The sigmoid midpoint deliberately deviates from Lucene's median:
//! `beta` anchors at the same 95th-percentile relevance boundary that
//! defines the base rate, so `P(relevant | raw = beta) = base_rate`
//! and a real query's top-ranked scores fall in the sigmoid's linear
//! region instead of its saturated tail.
//!
//! Pseudo-queries are built at several lengths around
//! `tokens_per_query`, and the per-length boundary and spread are
//! fitted with an affine model in the token count. The fitted slopes
//! travel with the parameters so scoring can translate the calibration
//! to the actual query length (`scaled_for_query_terms`), keeping long
//! real queries out of the sigmoid's saturated tail.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use uqa_core::DocId;
use uqa_storage::InvertedIndex;

use crate::bayesian_bm25::BayesianBM25Params;
use crate::bm25::{BM25Params, BM25Scorer};

const DEFAULT_N_SAMPLES: usize = 50;
const DEFAULT_TOKENS_PER_QUERY: usize = 5;
const DEFAULT_SEED: i64 = 42;
const MAX_COLLECTED_DOCS: usize = 10_000;
const BASE_RATE_MIN: f64 = 1e-6;
const BASE_RATE_MAX: f64 = 0.5;
const FALLBACK_BASE_RATE: f64 = 0.01;
/// Scores at or above this percentile of the sampled distribution count
/// as relevant; it defines both the base rate and the sigmoid midpoint.
const RELEVANCE_BOUNDARY_PERCENTILE: f64 = 0.95;
/// Below this many sampled scores the percentile boundary and the
/// standard deviation are noise; the estimator falls back instead of
/// fabricating a calibration.
const MIN_CALIBRATION_SAMPLES: usize = 10;
/// Floor on the coefficient of variation when deriving `alpha` from
/// the score spread. Near-identical samples would otherwise explode
/// `alpha = 1 / std` into a step function that erases ranking.
const MIN_RELATIVE_STD: f64 = 0.25;

#[derive(Debug, Clone, Copy)]
pub struct BayesianScoreEstimator {
    n_samples: usize,
    tokens_per_query: usize,
    seed: i64,
}

impl BayesianScoreEstimator {
    pub fn new(n_samples: usize, tokens_per_query: usize, seed: i64) -> Self {
        assert!(n_samples > 0, "n_samples must be positive, got {n_samples}");
        assert!(
            tokens_per_query > 0,
            "tokens_per_query must be positive, got {tokens_per_query}"
        );
        Self {
            n_samples,
            tokens_per_query,
            seed,
        }
    }

    pub fn n_samples(&self) -> usize {
        self.n_samples
    }

    pub fn tokens_per_query(&self) -> usize {
        self.tokens_per_query
    }

    /// The pseudo-query lengths this estimator calibrates across.
    pub fn calibration_lengths(&self) -> Vec<usize> {
        calibration_lengths(self.tokens_per_query)
    }

    /// Estimate from vocabulary-random pseudo-queries. Random terms
    /// rarely co-occur, so the fitted length slopes stay flat on
    /// sparse-vocabulary corpora; prefer
    /// [`Self::estimate_with_queries`] with document-sampled queries
    /// whenever document text is available.
    pub fn estimate(
        &self,
        index: &dyn InvertedIndex,
        field: &str,
        bm25_params: BM25Params,
    ) -> BayesianBM25Params {
        let sample_size = self
            .n_samples
            .checked_mul(self.tokens_per_query)
            .expect("n_samples * tokens_per_query must fit in usize");
        let vocabulary = index.vocabulary_terms(field);
        let sampled_terms = reservoir_sample(&vocabulary, sample_size, self.seed);

        let lengths = calibration_lengths(self.tokens_per_query);
        let mut queries: Vec<Vec<String>> = Vec::new();
        let mut cursor = 0;
        let mut query_index = 0;
        while cursor < sampled_terms.len() {
            let length = lengths[query_index % lengths.len()];
            query_index += 1;
            let end = (cursor + length).min(sampled_terms.len());
            if end - cursor < length {
                break;
            }
            queries.push(sampled_terms[cursor..end].to_vec());
            cursor = end;
        }

        self.estimate_with_queries(index, field, bm25_params, &queries)
    }

    /// Estimate from caller-provided pseudo-queries, grouped by their
    /// term counts for the length fit. Document-sampled queries (terms
    /// drawn from one document each) model the term co-occurrence of
    /// real queries, so the fitted boundary scales with query length
    /// the way real matching scores do.
    pub fn estimate_with_queries(
        &self,
        index: &dyn InvertedIndex,
        field: &str,
        bm25_params: BM25Params,
        queries: &[Vec<String>],
    ) -> BayesianBM25Params {
        let max_doc = index.doc_count() as usize;
        if max_doc == 0 || queries.is_empty() {
            return fallback_params(bm25_params);
        }

        let bm25_scorer = BM25Scorer::new(bm25_params, Arc::new(index.field_stats(field)));
        let mut scores_by_length: BTreeMap<usize, Vec<f64>> = BTreeMap::new();
        let mut base_rate_fractions = Vec::new();

        for query_terms in queries {
            if query_terms.is_empty() {
                continue;
            }
            let mut query_scores = collect_scores(index, field, query_terms, &bm25_scorer);
            if query_scores.is_empty() {
                continue;
            }

            query_scores.sort_by(|left, right| right.total_cmp(left));
            query_scores.truncate(max_doc.min(MAX_COLLECTED_DOCS));

            let mut sorted = query_scores.clone();
            sorted.sort_by(f64::total_cmp);
            let percentile_index = ((sorted.len() as f64) * RELEVANCE_BOUNDARY_PERCENTILE) as usize;
            let threshold = sorted[percentile_index.min(sorted.len() - 1)];
            let high_count = query_scores
                .iter()
                .filter(|score| **score >= threshold)
                .count();
            base_rate_fractions.push(high_count as f64 / max_doc as f64);
            scores_by_length
                .entry(query_terms.len())
                .or_default()
                .extend(query_scores);
        }

        // Per-length boundary and spread points for the affine fit.
        // Compressing the spread so best matches avoid the sigmoid's
        // upper tail was measured on SciFact and rejected: it costs
        // 1.6 to 4.9 NDCG@10 points by weakening the text signal's
        // fusion dominance, while a document matching many rare
        // coherent terms genuinely deserves a posterior near one.
        let mut boundary_points: Vec<(f64, f64)> = Vec::new();
        let mut spread_points: Vec<(f64, f64)> = Vec::new();
        let mut total_scores = 0;
        for (length, scores) in &mut scores_by_length {
            total_scores += scores.len();
            if scores.len() < MIN_CALIBRATION_SAMPLES {
                continue;
            }
            scores.sort_by(f64::total_cmp);
            let boundary_index = ((scores.len() as f64) * RELEVANCE_BOUNDARY_PERCENTILE) as usize;
            let boundary = scores[boundary_index.min(scores.len() - 1)];
            let mean = scores.iter().sum::<f64>() / scores.len() as f64;
            let variance = scores
                .iter()
                .map(|score| {
                    let difference = score - mean;
                    difference * difference
                })
                .sum::<f64>()
                / scores.len() as f64;
            let spread = variance.sqrt().max(MIN_RELATIVE_STD * mean.abs());
            boundary_points.push((*length as f64, boundary));
            spread_points.push((*length as f64, spread));
        }

        if boundary_points.is_empty() || total_scores < MIN_CALIBRATION_SAMPLES {
            return fallback_params(bm25_params);
        }

        let reference = self.tokens_per_query as f64;
        let (beta_intercept, beta_slope) = affine_fit(&boundary_points);
        let (sigma_intercept, sigma_slope) = affine_fit(&spread_points);
        let beta = beta_intercept + beta_slope * reference;
        let fallback_sigma =
            spread_points.iter().map(|(_, s)| *s).sum::<f64>() / spread_points.len() as f64;
        let mut sigma = sigma_intercept + sigma_slope * reference;
        if sigma <= 0.0 {
            sigma = fallback_sigma;
        }
        let alpha = if sigma > 0.0 { sigma.recip() } else { 1.0 };
        let base_rate = (base_rate_fractions.iter().sum::<f64>()
            / base_rate_fractions.len() as f64)
            .clamp(BASE_RATE_MIN, BASE_RATE_MAX);

        BayesianBM25Params {
            bm25: bm25_params,
            alpha,
            beta,
            base_rate,
            calibration_tokens: reference,
            beta_slope,
            sigma_slope,
        }
    }
}

/// Pseudo-query lengths bracketing the reference length, so the fit
/// interpolates for typical query lengths and extrapolates gently
/// outside the bracket.
fn calibration_lengths(tokens_per_query: usize) -> Vec<usize> {
    let mut lengths = vec![
        (tokens_per_query / 2).max(1),
        tokens_per_query,
        tokens_per_query * 2,
    ];
    lengths.dedup();
    lengths
}

/// Least-squares affine fit `y = intercept + slope * x`. A single
/// point yields a flat line through it.
fn affine_fit(points: &[(f64, f64)]) -> (f64, f64) {
    let n = points.len() as f64;
    let mean_x = points.iter().map(|(x, _)| *x).sum::<f64>() / n;
    let mean_y = points.iter().map(|(_, y)| *y).sum::<f64>() / n;
    let covariance: f64 = points
        .iter()
        .map(|(x, y)| (x - mean_x) * (y - mean_y))
        .sum();
    let variance: f64 = points.iter().map(|(x, _)| (x - mean_x).powi(2)).sum();
    if variance <= f64::EPSILON {
        return (mean_y, 0.0);
    }
    let slope = covariance / variance;
    (mean_y - slope * mean_x, slope)
}

impl Default for BayesianScoreEstimator {
    fn default() -> Self {
        Self::new(DEFAULT_N_SAMPLES, DEFAULT_TOKENS_PER_QUERY, DEFAULT_SEED)
    }
}

fn fallback_params(bm25: BM25Params) -> BayesianBM25Params {
    BayesianBM25Params {
        bm25,
        alpha: 1.0,
        beta: 0.0,
        base_rate: FALLBACK_BASE_RATE,
        ..BayesianBM25Params::default()
    }
}

fn reservoir_sample(terms: &[String], sample_size: usize, seed: i64) -> Vec<String> {
    let mut random = JavaRandom::new(seed);
    let mut reservoir = Vec::with_capacity(sample_size.min(terms.len()));
    for (index, term) in terms.iter().enumerate() {
        let seen_count = (index as u64) + 1;
        if reservoir.len() < sample_size {
            reservoir.push(term.clone());
        } else {
            let replacement = random.next_u64_bounded(seen_count);
            if replacement < sample_size as u64 {
                reservoir[replacement as usize].clone_from(term);
            }
        }
    }
    reservoir
}

fn collect_scores(
    index: &dyn InvertedIndex,
    field: &str,
    query_terms: &[String],
    scorer: &BM25Scorer,
) -> Vec<f64> {
    let posting_lists = index.get_posting_lists_bulk(field, query_terms);
    let idfs: Vec<f64> = posting_lists
        .iter()
        .map(|posting_list| scorer.idf(posting_list.len() as u64))
        .collect();
    let mut matching_terms = BTreeMap::<DocId, Vec<(usize, u64)>>::new();
    let mut candidate_ids = BTreeSet::<DocId>::new();

    for (term_index, posting_list) in posting_lists.iter().enumerate() {
        for entry in posting_list {
            candidate_ids.insert(entry.doc_id);
            matching_terms
                .entry(entry.doc_id)
                .or_default()
                .push((term_index, entry.payload.positions.len() as u64));
        }
    }

    let candidate_ids: Vec<DocId> = candidate_ids.into_iter().collect();
    let doc_lengths = index.get_doc_lengths_bulk(&candidate_ids, field);
    candidate_ids
        .into_iter()
        .map(|doc_id| {
            let doc_length = doc_lengths.get(&doc_id).copied().unwrap_or(0);
            matching_terms
                .get(&doc_id)
                .into_iter()
                .flatten()
                .map(|(term_index, term_frequency)| {
                    scorer.score_with_idf(*term_frequency, doc_length, idfs[*term_index])
                })
                .sum()
        })
        .collect()
}

/// `java.util.Random`'s 48-bit generator, used so a seed selects the
/// same vocabulary reservoir as Lucene's estimator.
#[derive(Debug, Clone, Copy)]
struct JavaRandom {
    state: u64,
}

impl JavaRandom {
    const MULTIPLIER: u64 = 0x0005_DEEC_E66D;
    const ADDEND: u64 = 0xB;
    const MASK: u64 = (1_u64 << 48) - 1;

    fn new(seed: i64) -> Self {
        Self {
            state: ((seed as u64) ^ Self::MULTIPLIER) & Self::MASK,
        }
    }

    fn next(&mut self, bits: u32) -> u32 {
        self.state = self
            .state
            .wrapping_mul(Self::MULTIPLIER)
            .wrapping_add(Self::ADDEND)
            & Self::MASK;
        (self.state >> (48 - bits)) as u32
    }

    fn next_i64(&mut self) -> i64 {
        let high = i64::from(self.next(32) as i32) << 32;
        high.wrapping_add(i64::from(self.next(32) as i32))
    }

    fn next_u64_bounded(&mut self, bound: u64) -> u64 {
        debug_assert!(bound > 0);
        loop {
            let bits = (self.next_i64() as u64) >> 1;
            let value = bits % bound;
            if bits - value <= i64::MAX as u64 - (bound - 1) {
                return value;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use uqa_analysis::analyzer::standard_analyzer;
    use uqa_storage::{InvertedIndex, MemoryInvertedIndex};

    use super::*;

    fn populated_index() -> MemoryInvertedIndex {
        let mut index = MemoryInvertedIndex::new(standard_analyzer("english"));
        for (doc_id, body) in [
            (1, "alpha beta gamma"),
            (2, "alpha alpha delta"),
            (3, "beta epsilon zeta"),
            (4, "gamma delta eta theta"),
            (5, "alpha theta iota kappa"),
        ] {
            index.add_document(
                doc_id,
                BTreeMap::from([("body".to_string(), body.to_string())]),
            );
        }
        index
    }

    /// A corpus with a shared compact vocabulary, so multi-term
    /// pseudo-queries at every calibration length hit plenty of
    /// documents and the per-length fit has real points.
    fn co_occurring_index() -> MemoryInvertedIndex {
        let vocabulary = [
            "alpha", "beta", "gamma", "delta", "epsilon", "zeta", "eta", "theta", "iota", "kappa",
        ];
        let mut index = MemoryInvertedIndex::new(standard_analyzer("english"));
        for doc_id in 0..40u64 {
            let words: Vec<&str> = (0..4)
                .map(|offset| vocabulary[(doc_id as usize * 3 + offset * 2) % vocabulary.len()])
                .collect();
            index.add_document(
                doc_id + 1,
                BTreeMap::from([("body".to_string(), words.join(" "))]),
            );
        }
        index
    }

    #[test]
    fn estimate_fits_query_length_slopes() {
        let index = co_occurring_index();
        let params =
            BayesianScoreEstimator::new(12, 4, 42).estimate(&index, "body", BM25Params::default());
        assert!(
            (params.calibration_tokens - 4.0).abs() < 1e-12,
            "reference length must be tokens_per_query, got {}",
            params.calibration_tokens
        );
        assert!(
            params.beta_slope > 0.0,
            "longer pseudo-queries must raise the boundary, got {}",
            params.beta_slope
        );
        assert!(params.alpha.is_finite() && params.alpha > 0.0);
        // Scaling to a longer query raises beta monotonically.
        let scaled = params.scaled_for_query_terms(12);
        assert!(scaled.beta > params.beta);
    }

    #[test]
    fn affine_fit_recovers_a_line() {
        let (intercept, slope) = affine_fit(&[(2.0, 5.0), (4.0, 9.0), (8.0, 17.0)]);
        assert!((intercept - 1.0).abs() < 1e-9, "intercept {intercept}");
        assert!((slope - 2.0).abs() < 1e-9, "slope {slope}");
        let (flat_intercept, flat_slope) = affine_fit(&[(5.0, 3.5)]);
        assert!((flat_intercept - 3.5).abs() < 1e-12);
        assert!(flat_slope.abs() < 1e-12);
    }

    #[test]
    fn empty_index_returns_lucene_fallback() {
        let index = MemoryInvertedIndex::new(standard_analyzer("english"));
        let params =
            BayesianScoreEstimator::default().estimate(&index, "body", BM25Params::default());
        assert_eq!(params.alpha, 1.0);
        assert_eq!(params.beta, 0.0);
        assert_eq!(params.base_rate, 0.01);
    }

    #[test]
    fn estimate_is_seed_reproducible_and_bounded() {
        let index = populated_index();
        let estimator = BayesianScoreEstimator::new(3, 2, 42);
        let first = estimator.estimate(&index, "body", BM25Params::default());
        let second = estimator.estimate(&index, "body", BM25Params::default());
        assert_eq!(first.alpha, second.alpha);
        assert_eq!(first.beta, second.beta);
        assert_eq!(first.base_rate, second.base_rate);
        assert!(first.alpha.is_finite() && first.alpha > 0.0);
        assert!(first.beta.is_finite());
        assert!((BASE_RATE_MIN..=BASE_RATE_MAX).contains(&first.base_rate));
    }

    #[test]
    fn java_random_matches_known_next_long_sequence() {
        let mut random = JavaRandom::new(42);
        assert_eq!(random.next_i64(), -5_025_562_857_975_149_833);
        assert_eq!(random.next_i64(), -5_843_495_416_241_995_736);
    }
}
