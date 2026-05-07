//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ports scorer/helper cases from `uqa/tests/test_external_prior.py`.

use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::{Duration, Utc};
use uqa_core::{IndexStats, Value};
use uqa_scoring::{
    authority_prior, recency_prior, BayesianBM25Params, ExternalPriorScorer, PriorFn,
};

fn stats() -> Arc<IndexStats> {
    let mut stats = IndexStats::new(100);
    stats.avg_doc_length = 10.0;
    stats.set_doc_freq("_default", "test", 10);
    Arc::new(stats)
}

fn approx_eq(a: f64, b: f64) {
    assert!((a - b).abs() < 1e-9, "expected {a} ~= {b}");
}

fn scorer(prior_fn: PriorFn) -> ExternalPriorScorer {
    ExternalPriorScorer::new(BayesianBM25Params::default(), stats(), prior_fn)
}

#[test]
fn score_with_neutral_prior() {
    let scorer = scorer(Arc::new(|_: &BTreeMap<String, Value>| 0.5));
    let score = scorer.score_with_prior(3, 10, 10, &BTreeMap::new());
    assert!(score > 0.0);
    assert!(score < 1.0);
}

#[test]
fn high_prior_boosts_score() {
    let neutral = scorer(Arc::new(|_: &BTreeMap<String, Value>| 0.5));
    let high = scorer(Arc::new(|_: &BTreeMap<String, Value>| 0.9));
    let base_score = neutral.score_with_prior(3, 10, 10, &BTreeMap::new());
    let boosted_score = high.score_with_prior(3, 10, 10, &BTreeMap::new());
    assert!(boosted_score > base_score);
}

#[test]
fn low_prior_reduces_score() {
    let neutral = scorer(Arc::new(|_: &BTreeMap<String, Value>| 0.5));
    let low = scorer(Arc::new(|_: &BTreeMap<String, Value>| 0.1));
    let base_score = neutral.score_with_prior(3, 10, 10, &BTreeMap::new());
    let reduced_score = low.score_with_prior(3, 10, 10, &BTreeMap::new());
    assert!(reduced_score < base_score);
}

#[test]
fn score_in_probability_range() {
    let scorer = scorer(Arc::new(|_: &BTreeMap<String, Value>| 0.7));
    let mut fields = BTreeMap::new();
    fields.insert("authority".into(), Value::Str("high".into()));
    let score = scorer.score_with_prior(2, 8, 10, &fields);
    assert!(score > 0.0);
    assert!(score < 1.0);
}

#[test]
fn recency_missing_field_returns_neutral() {
    let prior = recency_prior("timestamp", 30.0);
    approx_eq(prior(&BTreeMap::new()), 0.5);
}

#[test]
fn recency_recent_date_gives_high_prior() {
    let prior = recency_prior("timestamp", 30.0);
    let mut fields = BTreeMap::new();
    fields.insert("timestamp".into(), Value::Str(Utc::now().to_rfc3339()));
    assert!(prior(&fields) > 0.7);
}

#[test]
fn recency_old_date_gives_lower_prior() {
    let prior = recency_prior("timestamp", 30.0);
    let mut fields = BTreeMap::new();
    fields.insert(
        "timestamp".into(),
        Value::Str((Utc::now() - Duration::days(365)).to_rfc3339()),
    );
    assert!(prior(&fields) < 0.6);
}

#[test]
fn recency_invalid_date_returns_neutral() {
    let prior = recency_prior("timestamp", 30.0);
    let mut fields = BTreeMap::new();
    fields.insert("timestamp".into(), Value::Str("not-a-date".into()));
    approx_eq(prior(&fields), 0.5);
}

#[test]
fn authority_high() {
    let prior = authority_prior("level", None);
    let mut fields = BTreeMap::new();
    fields.insert("level".into(), Value::Str("high".into()));
    approx_eq(prior(&fields), 0.8);
}

#[test]
fn authority_medium() {
    let prior = authority_prior("level", None);
    let mut fields = BTreeMap::new();
    fields.insert("level".into(), Value::Str("medium".into()));
    approx_eq(prior(&fields), 0.6);
}

#[test]
fn authority_low() {
    let prior = authority_prior("level", None);
    let mut fields = BTreeMap::new();
    fields.insert("level".into(), Value::Str("low".into()));
    approx_eq(prior(&fields), 0.4);
}

#[test]
fn authority_missing_field_returns_neutral() {
    let prior = authority_prior("level", None);
    approx_eq(prior(&BTreeMap::new()), 0.5);
}

#[test]
fn authority_unknown_level_returns_neutral() {
    let prior = authority_prior("level", None);
    let mut fields = BTreeMap::new();
    fields.insert("level".into(), Value::Str("unknown".into()));
    approx_eq(prior(&fields), 0.5);
}

#[test]
fn authority_custom_levels() {
    let mut levels = BTreeMap::new();
    levels.insert("expert".into(), 0.95);
    levels.insert("novice".into(), 0.3);
    let prior = authority_prior("rank", Some(levels));
    let mut fields = BTreeMap::new();
    fields.insert("rank".into(), Value::Str("expert".into()));
    approx_eq(prior(&fields), 0.95);
    fields.insert("rank".into(), Value::Str("novice".into()));
    approx_eq(prior(&fields), 0.3);
}
