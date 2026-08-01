//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn col(name: &str) -> Expr {
    Expr::Column(name.into())
}

fn eq(name: &str, v: i64) -> Expr {
    Expr::Binary {
        op: BinaryOp::Equal,
        lhs: Box::new(col(name)),
        rhs: Box::new(Expr::Literal(Value::Int(v))),
    }
}

#[test]
fn equality_uses_distinct_count() {
    let stats = RelationStats::new(1000).with_column(
        "user_id",
        ColumnStats {
            distinct_count: 250,
            row_count: 1000,
            ..Default::default()
        },
    );
    let est = CardinalityEstimator::new();
    let sel = est.selectivity(&eq("user_id", 7), &stats).raw();
    assert!((sel - (1.0 / 250.0)).abs() < 1e-9);
    assert_eq!(est.estimate_rows(&eq("user_id", 7), &stats), 4);
}

#[test]
fn and_selectivity_multiplies() {
    let stats = RelationStats::new(1000).with_column(
        "uid",
        ColumnStats {
            distinct_count: 100,
            row_count: 1000,
            ..Default::default()
        },
    );
    let est = CardinalityEstimator::new();
    let pred = Expr::And(vec![eq("uid", 1), eq("uid", 2)]);
    let sel = est.selectivity(&pred, &stats).raw();
    assert!((sel - (0.01 * 0.01)).abs() < 1e-9);
}

#[test]
fn or_selectivity_uses_inclusion_exclusion() {
    let stats = RelationStats::new(1000).with_column(
        "uid",
        ColumnStats {
            distinct_count: 10,
            row_count: 1000,
            ..Default::default()
        },
    );
    let est = CardinalityEstimator::new();
    let pred = Expr::Or(vec![eq("uid", 1), eq("uid", 2)]);
    let sel = est.selectivity(&pred, &stats).raw();
    assert!((sel - 0.19).abs() < 1e-9);
}

#[test]
fn term_uses_doc_freq() {
    let stats = IndexStats::new(1000).with_doc_freq("body", "rust", 42);
    let est = CardinalityEstimator::new();
    let op = OperatorTree::Term {
        query: "rust".into(),
        field: Some("body".into()),
        scoring: None,
        top_k: None,
    };
    assert_eq!(est.estimate(&op, &stats), 42.0);
}

#[test]
fn vector_threshold_picks_tier() {
    assert_eq!(CardinalityEstimator::vector_selectivity(0.95), 0.01);
    assert_eq!(CardinalityEstimator::vector_selectivity(0.7), 0.05);
    assert_eq!(CardinalityEstimator::vector_selectivity(0.5), 0.1);
    assert_eq!(CardinalityEstimator::vector_selectivity(0.0), 0.2);
}

#[test]
fn complement_subtracts_from_n() {
    let stats = IndexStats::new(100).with_doc_freq("body", "x", 30);
    let est = CardinalityEstimator::new();
    let op = OperatorTree::Complement(Box::new(OperatorTree::Term {
        query: "x".into(),
        field: Some("body".into()),
        scoring: None,
        top_k: None,
    }));
    assert!((est.estimate(&op, &stats) - 70.0).abs() < 1e-9);
}

#[test]
fn entropy_lower_bound_clamps_intersection() {
    let mut cols = BTreeMap::new();
    cols.insert(
        "a".to_string(),
        ColumnStats {
            distinct_count: 4,
            row_count: 1000,
            ..Default::default()
        },
    );
    let est = CardinalityEstimator::new().with_column_stats(cols);
    let stats = IndexStats::new(1000);
    let op = OperatorTree::Intersect(vec![
        OperatorTree::Filter {
            field: "a".into(),
            predicate: Predicate::Equals(Value::Int(1)),
            source: None,
        },
        OperatorTree::Filter {
            field: "a".into(),
            predicate: Predicate::Equals(Value::Int(2)),
            source: None,
        },
    ]);
    let result = est.estimate(&op, &stats);
    assert!(result >= 1.0);
}

#[test]
fn label_selectivity_handles_empty_graph() {
    let gs = GraphStats::default();
    assert_eq!(gs.label_selectivity(None), 1.0);
    assert_eq!(gs.label_selectivity(Some("knows")), 1.0);
}

#[test]
fn pattern_match_falls_back_when_no_stats() {
    let est = CardinalityEstimator::new();
    let stats = IndexStats::new(100);
    let pattern = GraphPatternIR {
        vertex_patterns: vec![],
        edge_patterns: vec![],
    };
    let op = OperatorTree::PatternMatch {
        pattern,
        graph: "g".into(),
    };
    let r = est.estimate(&op, &stats);
    assert!(r > 0.0);
}
