//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bayesian BM25 with an external prior (Section 12.2 #6, Paper 3).
//!
//! Combines the BM25 likelihood with a document-level prior via log-odds
//! addition:
//!
//! ```text
//! logit(posterior) = logit(likelihood) + logit(prior)
//! ```
//!
//! The prior is a `Fn(&BTreeMap<String, Value>) -> f64` that maps a
//! document's field bag to a probability in `(0, 1)`. The bundled
//! [`recency_prior`] and [`authority_prior`] helpers cover common
//! time- and authority-based shapes.
//!
//! Numerical safety: probabilities are clamped to `[1e-10, 1 - 1e-10]`
//! before the logit transform, so the combined posterior is always
//! finite. A likelihood of `>= 1` saturates the logit at `+10`; a
//! likelihood of `<= 0` saturates at `-10`.

use std::collections::BTreeMap;
use std::sync::Arc;

use time::{format_description::well_known::Rfc3339, OffsetDateTime};
use uqa_core::{IndexStats, Value};

use crate::bayesian_bm25::{BayesianBM25Params, BayesianBM25Scorer};
use crate::error::invalid_input;
use crate::ScoringResult;

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
    ) -> ScoringResult<Self> {
        let bm25 = BayesianBM25Scorer::new(params, index_stats)?;
        Ok(Self {
            params,
            bm25,
            prior_fn,
        })
    }

    /// Compute a fused posterior with the external prior.
    pub fn score_with_prior(
        &self,
        term_freq: u64,
        doc_length: u64,
        doc_freq: u64,
        doc_fields: &BTreeMap<String, Value>,
    ) -> ScoringResult<f64> {
        let likelihood = self.bm25.score(term_freq, doc_length, doc_freq);
        let prior = (self.prior_fn)(doc_fields);
        if !prior.is_finite() || !(0.0..1.0).contains(&prior) || prior == 0.0 {
            return Err(invalid_input(format!(
                "external prior must be finite and in (0, 1), got {prior}"
            )));
        }

        let logit_likelihood = if likelihood > 0.0 && likelihood < 1.0 {
            (likelihood / (1.0 - likelihood)).ln()
        } else if likelihood >= 1.0 {
            10.0
        } else {
            -10.0
        };
        let logit_prior = (prior / (1.0 - prior)).ln();
        let logit_posterior = logit_likelihood + logit_prior;
        let posterior = 1.0 / (1.0 + (-logit_posterior).exp());
        if posterior.is_finite() {
            Ok(posterior)
        } else {
            Err(invalid_input("external-prior posterior is non-finite"))
        }
    }
}

// ---------------------------------------------------------------------
// Prior factories
// ---------------------------------------------------------------------

/// Recency-based prior. Documents with a more recent timestamp in
/// `field` receive higher prior probability via exponential decay.
/// Missing or malformed timestamps receive the neutral prior, `0.5`.
/// Future timestamps have zero age.
pub fn recency_prior(field: impl Into<String>, decay_days: f64) -> PriorFn {
    let field = field.into();
    Arc::new(move |fields: &BTreeMap<String, Value>| -> f64 {
        let Some(Value::Str(timestamp)) = fields.get(&field) else {
            return 0.5;
        };
        recency_at(timestamp, decay_days, OffsetDateTime::now_utc())
    })
}

fn recency_at(timestamp: &str, decay_days: f64, now: OffsetDateTime) -> f64 {
    let Some(elapsed) = elapsed_milliseconds(timestamp, now) else {
        return 0.5;
    };
    let age_days = (elapsed as f64 / 1_000.0 / 86_400.0).max(0.0);
    0.5 + 0.4 * (-age_days / decay_days).exp()
}

fn elapsed_milliseconds(timestamp: &str, now: OffsetDateTime) -> Option<i128> {
    if !matches!(timestamp.as_bytes().get(10), Some(b'T' | b't' | b' ')) {
        return None;
    }
    let leap = timestamp.get(17..19) == Some("60");
    let unicode_minus = timestamp
        .get(timestamp.len().saturating_sub(8)..)
        .is_some_and(|tail| tail.starts_with('−'));
    // Stored timestamps may spell a negative offset with U+2212 or carry an explicit
    // leap second and its fraction. Preserve those fields before parsing the instant.
    let mut normalized = [0_u8; 35]; // YYYY-MM-DDTHH:MM:SS.123456789+HH:MM
    let input = if leap || unicode_minus {
        let (head, tail) = timestamp.split_at_checked(19)?;
        normalized[..19].copy_from_slice(head.as_bytes());
        if leap {
            normalized[17..19].copy_from_slice(b"59");
        }
        let mut len = 19;
        let offset = if let Some(digits) = tail.strip_prefix('.') {
            let n = digits.bytes().take_while(u8::is_ascii_digit).count();
            if n == 0 {
                return None;
            }
            let keep = n.min(9);
            normalized[len] = b'.';
            len += 1;
            normalized[len..len + keep].copy_from_slice(&digits.as_bytes()[..keep]);
            len += keep;
            &digits[n..]
        } else {
            tail
        };
        let offset = if let Some(magnitude) = offset.strip_prefix('−') {
            normalized[len] = b'-';
            len += 1;
            magnitude
        } else {
            offset
        };
        let end = len + offset.len();
        normalized
            .get_mut(len..end)?
            .copy_from_slice(offset.as_bytes());
        std::str::from_utf8(&normalized[..end]).ok()?
    } else {
        timestamp
    };
    let published = OffsetDateTime::parse(input, &Rfc3339).ok()?;
    if published.offset().whole_hours().unsigned_abs() >= 24 {
        return None;
    }
    let mut elapsed = now - published;
    // A UTC civil day ends after its leap second. The elapsed fraction still belongs
    // to that second when the clock's time of day precedes its ordinary predecessor.
    if leap
        && now.unix_timestamp().rem_euclid(86_400) <= published.unix_timestamp().rem_euclid(86_400)
    {
        elapsed -= time::Duration::SECOND;
    }
    Some(elapsed.whole_milliseconds())
}

/// Authority-based prior. Maps categorical authority levels to prior
/// probabilities. The default mapping is:
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retained_recency_timestamps_preserve_leap_seconds_and_offset_spellings() {
        let now = OffsetDateTime::parse("2017-01-01T00:00:00.500Z", &Rfc3339).unwrap();
        for (input, expected) in [
            ("2016-12-31T23:59:60.125Z", Some(375)),
            ("2017-01-01T08:59:60.125+09:00", Some(375)),
            ("2016-12-30T12:15:60.125Z", Some(128_640_375)),
            ("2024-04-30T20:00:00.0005−04:00", Some(-231_292_799_500)),
            ("2024-04-30T00:00:00+24:00", None),
            ("2016-12-31T23:59:60.125Zextra", None),
        ] {
            assert_eq!(elapsed_milliseconds(input, now), expected, "{input}");
        }
    }

    fn stats(n: u64, avgdl: f64) -> Arc<IndexStats> {
        let mut s = IndexStats::default();
        s.total_docs = n;
        s.avg_doc_length = avgdl;
        Arc::new(s)
    }

    #[test]
    fn recency_uses_an_offset_instant_and_whole_elapsed_milliseconds() {
        let now = OffsetDateTime::parse("2024-05-01T00:00:00.001500Z", &Rfc3339).unwrap();
        let decay = 1.0 / 86_400_000.0;
        for input in [
            "2024-05-01T00:00:00.000499999Z",
            "2024-04-30t20:00:00.000499999-04:00",
            "2024-05-01 09:00:00.000499999+09:00",
        ] {
            let expected = 0.5 + 0.4 * (-1.0_f64).exp();
            assert!(
                (recency_at(input, decay, now) - expected).abs() < 1e-12,
                "{input}"
            );
        }
        assert_eq!(
            recency_at("2024-05-01T00:00:00.000500001Z", decay, now),
            0.9
        );
        assert_eq!(recency_at("2024-05-02T00:00:00Z", decay, now), 0.9);
        for invalid in [
            "now",
            "2024-05-01",
            "2024-05-01T00:00:00",
            "2024-05-01X00:00:00Z",
            "2024-02-30T12:00:00Z",
        ] {
            assert_eq!(recency_at(invalid, decay, now), 0.5, "{invalid}");
        }
    }

    #[test]
    fn prior_higher_than_neutral_lifts_posterior() {
        let prior = Arc::new(|_: &BTreeMap<String, Value>| 0.9_f64);
        let s = ExternalPriorScorer::new(BayesianBM25Params::default(), stats(1000, 10.0), prior)
            .unwrap();
        let map = BTreeMap::new();
        let with_prior = s.score_with_prior(3, 10, 50, &map).unwrap();
        let without = s.bm25.score(3, 10, 50);
        assert!(with_prior > without, "{with_prior} <= {without}");
    }

    #[test]
    fn neutral_prior_recovers_likelihood() {
        let prior = Arc::new(|_: &BTreeMap<String, Value>| 0.5_f64);
        let s = ExternalPriorScorer::new(BayesianBM25Params::default(), stats(1000, 10.0), prior)
            .unwrap();
        let map = BTreeMap::new();
        let with_prior = s.score_with_prior(3, 10, 50, &map).unwrap();
        let without = s.bm25.score(3, 10, 50);
        assert!((with_prior - without).abs() < 1e-9);
    }

    #[test]
    fn invalid_external_prior_is_an_error() {
        for invalid in [f64::NAN, f64::NEG_INFINITY, 0.0, 1.0, 2.0] {
            let prior = Arc::new(move |_: &BTreeMap<String, Value>| invalid);
            let scorer =
                ExternalPriorScorer::new(BayesianBM25Params::default(), stats(1000, 10.0), prior)
                    .unwrap();
            assert!(scorer
                .score_with_prior(3, 10, 50, &BTreeMap::new())
                .is_err());
        }
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
