//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use uqa_operators::{EdgePatternIR, VertexPatternIR};

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
fn vector_threshold_uses_continuous_dimension_aware_tail() {
    let low = CardinalityEstimator::vector_selectivity(0.5, 64);
    let high = CardinalityEstimator::vector_selectivity(0.7, 64);
    let higher_dimension = CardinalityEstimator::vector_selectivity(0.5, 256);
    assert!(low > high);
    assert!(low > higher_dimension);
    assert!((CardinalityEstimator::vector_selectivity(0.0, 64) - 0.5).abs() < 1e-6);
    assert_eq!(CardinalityEstimator::vector_selectivity(-1.0, 64), 1.0);
    assert_eq!(CardinalityEstimator::vector_selectivity(1.0, 64), 0.0);
}

#[test]
fn knn_cardinality_cannot_exceed_the_relation() {
    let stats = IndexStats::new(5);
    let estimator = CardinalityEstimator::new();
    let operator = OperatorTree::KNN {
        query_vector: vec![1.0, 0.0],
        k: 10,
        field: "embedding".into(),
    };
    assert_eq!(estimator.estimate(&operator, &stats), 5.0);
}

#[test]
fn vector_join_threshold_uses_the_same_continuous_tail_model() {
    let operand = || OperatorTree::KNN {
        query_vector: vec![1.0; 64],
        k: 20,
        field: "embedding".into(),
    };
    let join = |threshold| OperatorTree::VectorSimilarityJoin {
        left: Box::new(operand()),
        right: Box::new(operand()),
        threshold,
    };
    let mut stats = IndexStats::new(1_000);
    stats.dimensions = 64;
    let estimator = CardinalityEstimator::new();
    let permissive = estimator.estimate(&join(0.3), &stats);
    let selective = estimator.estimate(&join(0.8), &stats);
    stats.dimensions = 256;
    let higher_dimension = estimator.estimate(&join(0.3), &stats);

    assert!(permissive > selective);
    assert!(permissive > higher_dimension);
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

struct CountingGraphSampler {
    outgoing_calls: Arc<AtomicUsize>,
}

impl GraphStoreSampler for CountingGraphSampler {
    fn vertex_ids(&self) -> Vec<u64> {
        (0..10_001).collect()
    }

    fn outgoing_edges(&self, vid: u64) -> Vec<EdgeSample> {
        self.outgoing_calls.fetch_add(1, Ordering::SeqCst);
        vec![EdgeSample {
            target_id: (vid + 1) % 10_001,
            label: "cites".into(),
        }]
    }

    fn vertex_satisfies(&self, _vid: u64, constraint: &VertexConstraint) -> bool {
        constraint(&uqa_core::Vertex::new(0, "Paper"))
    }
}

#[test]
fn large_pattern_match_uses_the_bound_graph_sampler() {
    let calls = Arc::new(AtomicUsize::new(0));
    let sampler = CountingGraphSampler {
        outgoing_calls: Arc::clone(&calls),
    };
    let graph_stats = GraphStats {
        num_vertices: 10_001,
        num_edges: 10_001,
        avg_out_degree: 1.0,
        graph_name: "citations".into(),
        ..GraphStats::default()
    };
    let estimator = CardinalityEstimator::new()
        .with_graph_stats(graph_stats)
        .with_graph_store(Arc::new(sampler));
    let pattern = GraphPatternIR {
        vertex_patterns: vec![
            VertexPatternIR {
                variable: "a".into(),
                constraints: Vec::new(),
                label: None,
            },
            VertexPatternIR {
                variable: "b".into(),
                constraints: Vec::new(),
                label: None,
            },
        ],
        edge_patterns: vec![EdgePatternIR {
            source_var: "a".into(),
            target_var: "b".into(),
            label: Some("cites".into()),
            constraints: Vec::new(),
        }],
    };
    let result = estimator.estimate(
        &OperatorTree::PatternMatch {
            pattern,
            graph: "citations".into(),
        },
        &IndexStats::new(10_001),
    );

    assert_eq!(calls.load(Ordering::SeqCst), 100);
    assert_eq!(result, 10_001.0);
}
