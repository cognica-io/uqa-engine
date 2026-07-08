//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Per-field Bayesian BM25 scoring with weighted log-odds fusion
//! across fields (Section 12.2 #1, Paper 3).
//!
//! Each field has independent calibration parameters (`alpha, beta,
//! base_rate`) plus a fusion weight. Per-field posteriors combine via
//! a weight-normalized log-odds conjunction; an absent field
//! contributes the no-match prior floor. Padding with 0.5 instead
//! would assert a higher belief for unmatched fields than calibrated
//! matched posteriors reach on small corpora, ranking documents that
//! match more fields below documents that match fewer.

use std::sync::Arc;

use uqa_core::IndexStats;

use crate::bayesian::BayesianProbabilityTransform;
use crate::bayesian_bm25::{BayesianBM25Params, BayesianBM25Scorer};
use crate::prob::log_odds_conjunction_weighted;

/// One scored field configuration.
#[derive(Debug, Clone)]
pub struct FieldConfig {
    pub field: String,
    pub params: BayesianBM25Params,
    pub weight: f64,
}

pub struct MultiFieldBayesianScorer {
    fields: Vec<String>,
    scorers: Vec<BayesianBM25Scorer>,
    weights: Vec<f64>,
}

impl MultiFieldBayesianScorer {
    pub fn new(field_configs: Vec<FieldConfig>, stats: &Arc<IndexStats>) -> Self {
        let mut fields = Vec::with_capacity(field_configs.len());
        let mut scorers = Vec::with_capacity(field_configs.len());
        let mut weights = Vec::with_capacity(field_configs.len());
        for cfg in field_configs {
            fields.push(cfg.field);
            scorers.push(BayesianBM25Scorer::new(cfg.params, stats.clone()));
            weights.push(cfg.weight);
        }
        Self {
            fields,
            scorers,
            weights,
        }
    }

    /// Score one document. Each `*_per_field` map keys on field name
    /// and yields the term frequency / document length / document
    /// frequency for that field. Missing fields fall back to the
    /// no-match prior floor.
    pub fn score_document(
        &self,
        term_freq_per_field: &std::collections::BTreeMap<String, u64>,
        doc_length_per_field: &std::collections::BTreeMap<String, u64>,
        doc_freq_per_field: &std::collections::BTreeMap<String, u64>,
    ) -> f64 {
        let mut probs: Vec<f64> = Vec::with_capacity(self.fields.len());
        for (i, name) in self.fields.iter().enumerate() {
            let tf = *term_freq_per_field.get(name).unwrap_or(&0);
            let dl = *doc_length_per_field.get(name).unwrap_or(&1);
            let df = *doc_freq_per_field.get(name).unwrap_or(&1);
            if tf == 0 {
                probs.push(BayesianProbabilityTransform::no_match_prior());
            } else {
                probs.push(self.scorers[i].score(tf, dl, df));
            }
        }
        if probs.len() == 1 {
            return probs[0];
        }
        let total: f64 = self.weights.iter().sum();
        let normalized: Vec<f64> = if total > 0.0 {
            self.weights.iter().map(|w| w / total).collect()
        } else {
            self.weights.clone()
        };
        log_odds_conjunction_weighted(&probs, &normalized, 0.0).unwrap_or(0.5)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_field_returns_scorer_output() {
        let mut base = IndexStats::default();
        base.total_docs = 100;
        base.avg_doc_length = 50.0;
        let stats = Arc::new(base);
        let stats = &stats;
        let scorer = MultiFieldBayesianScorer::new(
            vec![FieldConfig {
                field: "title".into(),
                params: BayesianBM25Params::default(),
                weight: 1.0,
            }],
            stats,
        );
        let tf = std::collections::BTreeMap::from([("title".into(), 5u64)]);
        let dl = std::collections::BTreeMap::from([("title".into(), 50u64)]);
        let df = std::collections::BTreeMap::from([("title".into(), 10u64)]);
        let s = scorer.score_document(&tf, &dl, &df);
        assert!((0.0..=1.0).contains(&s));
        assert!(s > 0.5);
    }

    #[test]
    fn missing_field_uses_prior() {
        let mut base = IndexStats::default();
        base.total_docs = 100;
        base.avg_doc_length = 50.0;
        let stats = Arc::new(base);
        let stats = &stats;
        let scorer = MultiFieldBayesianScorer::new(
            vec![
                FieldConfig {
                    field: "title".into(),
                    params: BayesianBM25Params::default(),
                    weight: 1.0,
                },
                FieldConfig {
                    field: "body".into(),
                    params: BayesianBM25Params::default(),
                    weight: 1.0,
                },
            ],
            stats,
        );
        // Only `title` has frequency data; `body` falls back to the
        // no-match prior floor.
        let tf = std::collections::BTreeMap::from([("title".into(), 5u64)]);
        let dl = std::collections::BTreeMap::from([("title".into(), 50u64)]);
        let df = std::collections::BTreeMap::from([("title".into(), 10u64)]);
        let s = scorer.score_document(&tf, &dl, &df);
        assert!((0.0..=1.0).contains(&s));
    }
}
