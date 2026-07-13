//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Corpus-driven parameter estimation for query-level Bayesian BM25.
//!
//! The estimator mirrors Lucene's `BayesianScoreEstimator`: it reservoir
//! samples a field's indexed vocabulary, builds OR pseudo-queries, gathers
//! their raw BM25 score distributions, and derives the sigmoid midpoint,
//! scale, and relevance base rate.

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

    pub fn estimate(
        &self,
        index: &dyn InvertedIndex,
        field: &str,
        bm25_params: BM25Params,
    ) -> BayesianBM25Params {
        let max_doc = index.doc_count() as usize;
        if max_doc == 0 {
            return fallback_params(bm25_params);
        }

        let sample_size = self
            .n_samples
            .checked_mul(self.tokens_per_query)
            .expect("n_samples * tokens_per_query must fit in usize");
        let vocabulary = index.vocabulary_terms(field);
        let sampled_terms = reservoir_sample(&vocabulary, sample_size, self.seed);
        if sampled_terms.is_empty() {
            return fallback_params(bm25_params);
        }

        let bm25_scorer = BM25Scorer::new(bm25_params, Arc::new(index.field_stats(field)));
        let mut all_scores = Vec::new();
        let mut base_rate_fractions = Vec::new();

        for query_terms in sampled_terms.chunks(self.tokens_per_query) {
            let mut query_scores = collect_scores(index, field, query_terms, &bm25_scorer);
            if query_scores.is_empty() {
                continue;
            }

            query_scores.sort_by(|left, right| right.total_cmp(left));
            query_scores.truncate(max_doc.min(MAX_COLLECTED_DOCS));

            let mut sorted = query_scores.clone();
            sorted.sort_by(f64::total_cmp);
            let percentile_index = ((sorted.len() as f64) * 0.95) as usize;
            let threshold = sorted[percentile_index.min(sorted.len() - 1)];
            let high_count = query_scores
                .iter()
                .filter(|score| **score >= threshold)
                .count();
            base_rate_fractions.push(high_count as f64 / max_doc as f64);
            all_scores.extend(query_scores);
        }

        if all_scores.is_empty() {
            return fallback_params(bm25_params);
        }

        all_scores.sort_by(f64::total_cmp);
        let beta = all_scores[all_scores.len() / 2];
        let mean = all_scores.iter().sum::<f64>() / all_scores.len() as f64;
        let variance = all_scores
            .iter()
            .map(|score| {
                let difference = score - mean;
                difference * difference
            })
            .sum::<f64>()
            / all_scores.len() as f64;
        let standard_deviation = variance.sqrt();
        let alpha = if standard_deviation > 0.0 {
            standard_deviation.recip()
        } else {
            1.0
        };
        let base_rate = (base_rate_fractions.iter().sum::<f64>()
            / base_rate_fractions.len() as f64)
            .clamp(BASE_RATE_MIN, BASE_RATE_MAX);

        BayesianBM25Params {
            bm25: bm25_params,
            alpha,
            beta,
            base_rate,
        }
    }
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
