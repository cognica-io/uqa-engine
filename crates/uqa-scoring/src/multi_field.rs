//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Per-field Bayesian BM25 scoring with weighted log-odds fusion
//! across fields (Section 12.2 #1, Paper 3).
//!
//! Each field has independent calibration parameters (`alpha, beta,
//! base_rate`) plus a fusion weight. Per-field posteriors are unwrapped
//! into prior-free evidence logits (`logit(p_i) - logit(r_i)`), floored
//! by softplus (Remark 6.5.4: a matching field never counts against a
//! document beyond the prior), the weighted evidence is
//! confidence-scaled by `sqrt(n)`, and the weighted prior enters
//! exactly once. An absent field contributes zero evidence, so a
//! document matching nothing rests at the prior.

use std::sync::Arc;

use uqa_core::IndexStats;

use crate::bayesian_bm25::{BayesianBM25Params, BayesianBM25Scorer};
use crate::error::invalid_input;
use crate::prob::sigmoid;
use crate::ScoringResult;

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
    pub fn new(field_configs: Vec<FieldConfig>, stats: &Arc<IndexStats>) -> ScoringResult<Self> {
        let total_weight = field_configs.iter().try_fold(0.0, |total, config| {
            if config.field.is_empty() {
                return Err(invalid_input("multi-field field name must not be empty"));
            }
            if !config.weight.is_finite() || config.weight < 0.0 {
                return Err(invalid_input(format!(
                    "multi-field weight for {:?} must be finite and non-negative, got {}",
                    config.field, config.weight
                )));
            }
            let next = total + config.weight;
            if next.is_finite() {
                Ok(next)
            } else {
                Err(invalid_input("multi-field weight sum overflowed"))
            }
        })?;
        if !field_configs.is_empty() && total_weight <= 0.0 {
            return Err(invalid_input(
                "multi-field weights must have a positive finite sum",
            ));
        }
        let mut fields = Vec::with_capacity(field_configs.len());
        let mut scorers = Vec::with_capacity(field_configs.len());
        let mut weights = Vec::with_capacity(field_configs.len());
        for cfg in field_configs {
            fields.push(cfg.field);
            scorers.push(BayesianBM25Scorer::new(cfg.params, stats.clone())?);
            weights.push(cfg.weight / total_weight);
        }
        Ok(Self {
            fields,
            scorers,
            weights,
        })
    }

    /// Score one document. Each `*_per_field` map keys on field name
    /// and yields the term frequency / document length / document
    /// frequency for that field. Missing fields contribute sparse
    /// absence rather than a synthetic probability.
    pub fn score_document(
        &self,
        term_freq_per_field: &std::collections::BTreeMap<String, u64>,
        doc_length_per_field: &std::collections::BTreeMap<String, u64>,
        doc_freq_per_field: &std::collections::BTreeMap<String, u64>,
    ) -> f64 {
        if self.fields.is_empty() {
            return 0.5;
        }
        let mut probabilities: Vec<Option<f64>> = Vec::with_capacity(self.fields.len());
        for (i, name) in self.fields.iter().enumerate() {
            let tf = *term_freq_per_field.get(name).unwrap_or(&0);
            let dl = *doc_length_per_field.get(name).unwrap_or(&1);
            let df = *doc_freq_per_field.get(name).unwrap_or(&1);
            if tf == 0 {
                probabilities.push(None);
            } else {
                probabilities.push(Some(self.scorers[i].score(tf, dl, df)));
            }
        }
        if probabilities.len() == 1 {
            return probabilities[0].unwrap_or(0.5);
        }
        let evidence_sum: f64 = probabilities
            .iter()
            .zip(&self.weights)
            .zip(&self.scorers)
            .filter_map(|((probability, weight), scorer)| {
                probability.map(|probability| {
                    weight * softplus(lucene_logit(probability) - field_prior_logit(scorer))
                })
            })
            .sum();
        let prior_logit: f64 = self
            .weights
            .iter()
            .zip(&self.scorers)
            .map(|(weight, scorer)| weight * field_prior_logit(scorer))
            .sum();
        sigmoid(evidence_sum * (probabilities.len() as f64).sqrt() + prior_logit)
    }
}

fn softplus(value: f64) -> f64 {
    if value > 20.0 {
        value
    } else {
        value.exp().ln_1p()
    }
}

/// `logit(base_rate)` of a field's calibration; zero when the field's
/// prior is disabled (`base_rate == 0`), i.e. the posterior is already
/// prior-free evidence.
fn field_prior_logit(scorer: &BayesianBM25Scorer) -> f64 {
    let base_rate = scorer.params.base_rate;
    if base_rate > 0.0 {
        lucene_logit(base_rate)
    } else {
        0.0
    }
}

fn lucene_logit(probability: f64) -> f64 {
    let clamped = probability.clamp(1e-7, 1.0 - 1e-7);
    (clamped / (1.0 - clamped)).ln()
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
        )
        .unwrap();
        let tf = std::collections::BTreeMap::from([("title".into(), 5u64)]);
        let dl = std::collections::BTreeMap::from([("title".into(), 50u64)]);
        let df = std::collections::BTreeMap::from([("title".into(), 10u64)]);
        let s = scorer.score_document(&tf, &dl, &df);
        assert!((0.0..=1.0).contains(&s));
        assert!(s > 0.5);
    }

    #[test]
    fn missing_field_contributes_sparse_absence() {
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
        )
        .unwrap();
        // Only `title` has frequency data; `body` contributes zero.
        let tf = std::collections::BTreeMap::from([("title".into(), 5u64)]);
        let dl = std::collections::BTreeMap::from([("title".into(), 50u64)]);
        let df = std::collections::BTreeMap::from([("title".into(), 10u64)]);
        let s = scorer.score_document(&tf, &dl, &df);
        assert!((0.0..=1.0).contains(&s));
    }

    #[test]
    fn constructor_rejects_invalid_field_weights() {
        let stats = Arc::new(IndexStats::default());
        let config = |field: &str, weight: f64| FieldConfig {
            field: field.to_string(),
            params: BayesianBM25Params::default(),
            weight,
        };
        assert!(MultiFieldBayesianScorer::new(vec![config("", 1.0)], &stats).is_err());
        assert!(MultiFieldBayesianScorer::new(vec![config("title", f64::NAN)], &stats).is_err());
        assert!(MultiFieldBayesianScorer::new(vec![config("title", -1.0)], &stats).is_err());
        assert!(MultiFieldBayesianScorer::new(
            vec![config("title", 0.0), config("body", 0.0)],
            &stats,
        )
        .is_err());
    }
}
