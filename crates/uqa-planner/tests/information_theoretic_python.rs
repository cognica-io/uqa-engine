//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Port of Python `test_information_theoretic.py`.

use std::collections::BTreeMap;

use uqa_core::{IndexStats, Predicate, Value};
use uqa_operators::OperatorTree;
use uqa_planner::{
    column_entropy, entropy_cardinality_lower_bound, mutual_information_estimate,
    CardinalityEstimator, ColumnStats,
};

fn make_stats(ndv: u64, mcv_frequencies: Vec<f64>) -> ColumnStats {
    ColumnStats {
        distinct_count: ndv,
        null_count: 0,
        min_value: Some(Value::Int(0)),
        max_value: Some(Value::Int(ndv as i64)),
        row_count: 1000,
        mcv_values: (0..mcv_frequencies.len())
            .map(|i| Value::Str(format!("mcv_{i}")))
            .collect(),
        mcv_frequencies,
        ..ColumnStats::default()
    }
}

#[test]
fn test_column_entropy_uniform() {
    assert!((column_entropy(&make_stats(8, Vec::new())) - 3.0).abs() < 0.01);
}

#[test]
fn test_column_entropy_single_value() {
    assert_eq!(column_entropy(&make_stats(1, Vec::new())), 0.0);
}

#[test]
fn test_column_entropy_zero() {
    assert_eq!(column_entropy(&make_stats(0, Vec::new())), 0.0);
}

#[test]
fn test_column_entropy_with_mcv() {
    let entropy = column_entropy(&make_stats(4, vec![0.4, 0.3]));
    assert!(entropy < 2.0);
    assert!(entropy > 0.0);
}

#[test]
fn test_mutual_information_zero() {
    let x = make_stats(10, Vec::new());
    let y = make_stats(10, Vec::new());
    assert_eq!(mutual_information_estimate(&x, &y, 0.0), 0.0);
}

#[test]
fn test_mutual_information_positive() {
    let x = make_stats(10, Vec::new());
    let y = make_stats(10, Vec::new());
    assert!(mutual_information_estimate(&x, &y, 0.1) >= 0.0);
}

#[test]
fn test_mutual_information_correlation() {
    let x = make_stats(4, Vec::new());
    let y = make_stats(4, Vec::new());
    let correlated = mutual_information_estimate(&x, &y, 0.01);
    let independent = mutual_information_estimate(&x, &y, 0.5);
    assert!(correlated > independent);
}

#[test]
fn test_entropy_lower_bound_single() {
    assert!((entropy_cardinality_lower_bound(1000.0, &[3.0]) - 125.0).abs() < 0.01);
}

#[test]
fn test_entropy_lower_bound_multiple() {
    assert!((entropy_cardinality_lower_bound(1000.0, &[3.0, 3.0]) - 15.625).abs() < 0.01);
}

#[test]
fn test_entropy_lower_bound_empty() {
    assert_eq!(entropy_cardinality_lower_bound(1000.0, &[]), 1.0);
}

#[test]
fn test_entropy_lower_bound_zero_n() {
    assert_eq!(entropy_cardinality_lower_bound(0.0, &[3.0]), 1.0);
}

#[test]
fn test_entropy_lower_bound_floor() {
    assert_eq!(entropy_cardinality_lower_bound(10.0, &[20.0]), 1.0);
}

#[test]
fn test_entropy_lower_bound_in_intersection() {
    let mut column_stats = BTreeMap::new();
    column_stats.insert("age".into(), make_stats(50, Vec::new()));
    column_stats.insert("dept".into(), make_stats(5, Vec::new()));
    let estimator = CardinalityEstimator::new().with_column_stats(column_stats.clone());
    let stats = IndexStats::new(1000);
    let op = OperatorTree::Intersect(vec![
        OperatorTree::Filter {
            field: "age".into(),
            predicate: Predicate::Equals(Value::Int(25)),
            source: None,
        },
        OperatorTree::Filter {
            field: "dept".into(),
            predicate: Predicate::Equals(Value::Int(3)),
            source: None,
        },
    ]);
    let card = estimator.estimate(&op, &stats);
    let lower = entropy_cardinality_lower_bound(
        1000.0,
        &[
            column_entropy(column_stats.get("age").unwrap()),
            column_entropy(column_stats.get("dept").unwrap()),
        ],
    );
    assert!(card >= 1.0);
    assert!(card >= lower);
}

#[test]
fn test_entropy_clamping_in_filter_selectivity() {
    let mut column_stats = BTreeMap::new();
    column_stats.insert("color".into(), make_stats(4, Vec::new()));
    let estimator = CardinalityEstimator::new().with_column_stats(column_stats);
    let selectivity =
        estimator.filter_selectivity("color", &Predicate::Equals(Value::Str("red".into())), 100.0);
    assert!(selectivity >= 0.25 - 1e-9);
}

#[test]
fn test_entropy_clamping_does_not_raise_high_selectivity() {
    let mut column_stats = BTreeMap::new();
    column_stats.insert("score".into(), make_stats(4, Vec::new()));
    let estimator = CardinalityEstimator::new().with_column_stats(column_stats);
    let selectivity =
        estimator.filter_selectivity("score", &Predicate::GreaterThan(Value::Int(25)), 100.0);
    assert!(selectivity >= 0.25);
    assert!(selectivity <= 1.0);
}
