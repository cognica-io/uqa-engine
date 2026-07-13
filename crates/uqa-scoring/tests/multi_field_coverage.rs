//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Focused implementation of `TestMultiFieldBayesianScorer` from `test_multi_field`.

use std::collections::BTreeMap;
use std::sync::Arc;

use uqa_core::IndexStats;
use uqa_scoring::{BayesianBM25Params, FieldConfig, MultiFieldBayesianScorer};

fn stats() -> Arc<IndexStats> {
    let mut stats = IndexStats::default();
    stats.total_docs = 100;
    stats.avg_doc_length = 10.0;
    Arc::new(stats)
}

fn map(items: &[(&str, u64)]) -> BTreeMap<String, u64> {
    items
        .iter()
        .map(|(field, value)| ((*field).to_string(), *value))
        .collect()
}

#[test]
fn single_field_score_is_probability() {
    let scorer = MultiFieldBayesianScorer::new(
        vec![FieldConfig {
            field: "title".into(),
            params: BayesianBM25Params::default(),
            weight: 1.0,
        }],
        &stats(),
    );
    let score = scorer.score_document(
        &map(&[("title", 3)]),
        &map(&[("title", 10)]),
        &map(&[("title", 10)]),
    );
    assert!(score > 0.0 && score < 1.0);
}

#[test]
fn two_fields_score_higher_than_one() {
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
                weight: 0.5,
            },
        ],
        &stats(),
    );
    let one_field = scorer.score_document(
        &map(&[("title", 3), ("body", 0)]),
        &map(&[("title", 10), ("body", 100)]),
        &map(&[("title", 10), ("body", 50)]),
    );
    let two_fields = scorer.score_document(
        &map(&[("title", 3), ("body", 5)]),
        &map(&[("title", 10), ("body", 100)]),
        &map(&[("title", 10), ("body", 50)]),
    );
    assert!(two_fields > one_field);
}

#[test]
fn zero_tf_gives_neutral_sparse_absence() {
    let scorer = MultiFieldBayesianScorer::new(
        vec![FieldConfig {
            field: "title".into(),
            params: BayesianBM25Params::default(),
            weight: 1.0,
        }],
        &stats(),
    );
    let score = scorer.score_document(
        &map(&[("title", 0)]),
        &map(&[("title", 10)]),
        &map(&[("title", 10)]),
    );
    assert!((score - 0.5).abs() < 1e-12, "{score} vs neutral absence");
}
