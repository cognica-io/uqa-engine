//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bayesian BM25 with an external prior (Section 12.2 #6, Paper 3).
//!
//! Mirrors `uqa.scoring.external_prior`. Combines the BM25 likelihood
//! with a document-level prior via log-odds addition:
//!
//! ```text
//! logit(posterior) = logit(likelihood) + logit(prior)
//! ```
//!
//! The prior is a `Fn(&BTreeMap<String, Value>) -> f64` that maps a
//! document's field bag to a probability in `(0, 1)`. The bundled
//! [`recency_prior`] / [`authority_prior`] helpers cover the common
//! shapes from the Python reference.
//!
//! Numerical safety: probabilities are clamped to `[1e-10, 1 - 1e-10]`
//! before the logit transform, so the combined posterior is always
//! finite. A likelihood of `>= 1` saturates the logit at `+10`; a
//! likelihood of `<= 0` saturates at `-10` -- same constants as
//! Python's reference for cross-language parity.

use std::collections::BTreeMap;
use std::sync::Arc;

use uqa_core::{IndexStats, Value};

use crate::bayesian_bm25::{BayesianBM25Params, BayesianBM25Scorer};

/// User-supplied prior. Returns a probability in `(0, 1)`.
pub type PriorFn = Arc<dyn Fn(&BTreeMap<String, Value>) -> f64 + Send + Sync>;

pub struct ExternalPriorScorer {
    pub params: BayesianBM25Params,
    pub bm25: BayesianBM25Scorer,
    prior_fn: PriorFn,
}

impl ExternalPriorScorer {
    pub fn new(
        params: BayesianBM25Params,
        index_stats: Arc<IndexStats>,
        prior_fn: PriorFn,
    ) -> Self {
        let bm25 = BayesianBM25Scorer::new(params, index_stats);
        Self {
            params,
            bm25,
            prior_fn,
        }
    }

    /// Fused posterior with the external prior. Mirrors
    /// `score_with_prior` in the Python reference.
    pub fn score_with_prior(
        &self,
        term_freq: u64,
        doc_length: u64,
        doc_freq: u64,
        doc_fields: &BTreeMap<String, Value>,
    ) -> f64 {
        let likelihood = self.bm25.score(term_freq, doc_length, doc_freq);
        let prior = (self.prior_fn)(doc_fields).clamp(1e-10, 1.0 - 1e-10);

        let logit_likelihood = if likelihood > 0.0 && likelihood < 1.0 {
            (likelihood / (1.0 - likelihood)).ln()
        } else if likelihood >= 1.0 {
            10.0
        } else {
            -10.0
        };
        let logit_prior = (prior / (1.0 - prior)).ln();
        let logit_posterior = logit_likelihood + logit_prior;
        1.0 / (1.0 + (-logit_posterior).exp())
    }
}

// ---------------------------------------------------------------------
// Prior factories
// ---------------------------------------------------------------------

/// Recency-based prior. Documents with a more recent timestamp in
/// `field` receive higher prior probability via exponential decay.
/// Returns `0.5` (neutral) when the field is missing, malformed, or
/// the timestamp lies in the future.
pub fn recency_prior(field: impl Into<String>, decay_days: f64) -> PriorFn {
    let field = field.into();
    Arc::new(move |fields: &BTreeMap<String, Value>| -> f64 {
        let Some(val) = fields.get(&field) else {
            return 0.5;
        };
        let Some(ts) = parse_timestamp(val) else {
            return 0.5;
        };
        let now = chrono::Utc::now();
        let age_days = ((now - ts).num_milliseconds() as f64 / 1000.0 / 86_400.0).max(0.0);
        0.5 + 0.4 * (-age_days / decay_days).exp()
    })
}

/// Authority-based prior. Maps categorical authority levels to prior
/// probabilities. The default mapping mirrors the Python reference:
/// `high -> 0.8`, `medium -> 0.6`, `low -> 0.4`. Returns `0.5`
/// (neutral) when the field is missing or unrecognized.
pub fn authority_prior(field: impl Into<String>, levels: Option<BTreeMap<String, f64>>) -> PriorFn {
    let field = field.into();
    let mapping = levels.unwrap_or_else(|| {
        let mut m = BTreeMap::new();
        m.insert("high".to_string(), 0.8);
        m.insert("medium".to_string(), 0.6);
        m.insert("low".to_string(), 0.4);
        m
    });
    Arc::new(move |fields: &BTreeMap<String, Value>| -> f64 {
        let Some(val) = fields.get(&field) else {
            return 0.5;
        };
        let key = match val {
            Value::Str(s) => s.clone(),
            Value::Bool(b) => b.to_string(),
            Value::Int(i) => i.to_string(),
            Value::Float(f) => f.to_string(),
            _ => return 0.5,
        };
        mapping.get(&key).copied().unwrap_or(0.5)
    })
}

fn parse_timestamp(v: &Value) -> Option<chrono::DateTime<chrono::Utc>> {
    match v {
        Value::Str(s) => chrono::DateTime::parse_from_rfc3339(s)
            .ok()
            .map(|dt| dt.with_timezone(&chrono::Utc)),
        _ => None,
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
    fn prior_higher_than_neutral_lifts_posterior() {
        let prior = Arc::new(|_: &BTreeMap<String, Value>| 0.9_f64);
        let s = ExternalPriorScorer::new(BayesianBM25Params::default(), stats(1000, 10.0), prior);
        let map = BTreeMap::new();
        let with_prior = s.score_with_prior(3, 10, 50, &map);
        let without = s.bm25.score(3, 10, 50);
        assert!(with_prior > without, "{with_prior} <= {without}");
    }

    #[test]
    fn neutral_prior_recovers_likelihood() {
        let prior = Arc::new(|_: &BTreeMap<String, Value>| 0.5_f64);
        let s = ExternalPriorScorer::new(BayesianBM25Params::default(), stats(1000, 10.0), prior);
        let map = BTreeMap::new();
        let with_prior = s.score_with_prior(3, 10, 50, &map);
        let without = s.bm25.score(3, 10, 50);
        assert!((with_prior - without).abs() < 1e-9);
    }

    #[test]
    fn authority_prior_maps_known_levels() {
        let p = authority_prior("rank", None);
        let mut row = BTreeMap::new();
        row.insert("rank".to_string(), Value::Str("high".into()));
        assert!((p(&row) - 0.8).abs() < 1e-9);
        row.insert("rank".to_string(), Value::Str("low".into()));
        assert!((p(&row) - 0.4).abs() < 1e-9);
        row.insert("rank".to_string(), Value::Str("unknown".into()));
        assert!((p(&row) - 0.5).abs() < 1e-9);
    }
}
