//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! End-to-end exercise of the lower → optimise → execute pipeline.
//! `WHERE text_match(...)` and the full WHERE-based boolean algebra now
//! flow through `QueryOptimizer` instead of bypassing the operator tree
//! entirely. The driver re-uses the engine's existing `text_match` /
//! `knn_match` leaves so every access path shares one implementation.

use std::collections::BTreeMap;
use std::sync::Arc;

use uqa_core::{Edge, Predicate, Value, Vertex};
use uqa_engine::operator_tree_bridge::EngineDriver;
use uqa_engine::Engine;
use uqa_fusion::{AttentionFusion, LearnedFusion};
use uqa_operators::{
    AggState, AggregationMonoid, CountMonoid, GatingSpec, OperatorTree, ProbBoolMode, SumMonoid,
    TextScoringMode,
};
use uqa_planner::executor::OperatorTreeDriver;
use uqa_sql::SQLError;

fn engine_with_corpus() -> Engine {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE notes (id INTEGER PRIMARY KEY, title TEXT, body TEXT, year INTEGER)",
        &[],
    )
    .unwrap();
    eng.sql(
        "CREATE INDEX notes_fts_idx ON notes USING gin (title, body)",
        &[],
    )
    .unwrap();
    eng.sql(
        "INSERT INTO notes (id, title, body, year) VALUES \
         (1, 'rust async', 'futures and tokio', 2024), \
         (2, 'rust embedded', 'no_std and cortex_m', 2025), \
         (3, 'python web', 'flask and django', 2024), \
         (4, 'rust web', 'axum tokio hyper', 2025)",
        &[],
    )
    .unwrap();
    eng
}

#[test]
fn text_match_through_optimiser_returns_matching_docs() {
    let eng = engine_with_corpus();
    let r = eng
        .sql(
            "SELECT id FROM notes WHERE text_match(body, 'tokio') ORDER BY id",
            &[],
        )
        .unwrap();
    let ids: Vec<i64> = r
        .rows
        .iter()
        .map(|row| match row.get("id") {
            Some(Value::Int(n)) => *n,
            other => panic!("unexpected id: {other:?}"),
        })
        .collect();
    assert_eq!(ids, vec![1, 4]);
}

#[test]
fn intersect_of_text_match_and_filter_runs_through_optimiser() {
    let eng = engine_with_corpus();
    let r = eng
        .sql(
            "SELECT id FROM notes WHERE text_match(body, 'tokio') AND year = 2025 ORDER BY id",
            &[],
        )
        .unwrap();
    let ids: Vec<i64> = r
        .rows
        .iter()
        .map(|row| match row.get("id") {
            Some(Value::Int(n)) => *n,
            other => panic!("unexpected id: {other:?}"),
        })
        .collect();
    assert_eq!(ids, vec![4]);
}

#[test]
fn union_of_two_text_match_signals_runs_through_optimiser() {
    let eng = engine_with_corpus();
    let r = eng
        .sql(
            "SELECT id FROM notes WHERE text_match(title, 'rust') OR text_match(body, 'flask') ORDER BY id",
            &[],
        )
        .unwrap();
    let ids: Vec<i64> = r
        .rows
        .iter()
        .map(|row| match row.get("id") {
            Some(Value::Int(n)) => *n,
            other => panic!("unexpected id: {other:?}"),
        })
        .collect();
    assert_eq!(ids, vec![1, 2, 3, 4]);
}

#[test]
fn negation_through_complement_returns_unmatched_docs() {
    let eng = engine_with_corpus();
    let r = eng
        .sql(
            "SELECT id FROM notes WHERE NOT text_match(title, 'python') ORDER BY id",
            &[],
        )
        .unwrap();
    let ids: Vec<i64> = r
        .rows
        .iter()
        .map(|row| match row.get("id") {
            Some(Value::Int(n)) => *n,
            other => panic!("unexpected id: {other:?}"),
        })
        .collect();
    assert_eq!(ids, vec![1, 2, 4]);
}

#[test]
fn pure_column_filter_lowers_to_filter_node() {
    let eng = engine_with_corpus();
    let r = eng
        .sql("SELECT id FROM notes WHERE year = 2024 ORDER BY id", &[])
        .unwrap();
    let ids: Vec<i64> = r
        .rows
        .iter()
        .map(|row| match row.get("id") {
            Some(Value::Int(n)) => *n,
            other => panic!("unexpected id: {other:?}"),
        })
        .collect();
    assert_eq!(ids, vec![1, 3]);
}

#[test]
fn driver_propagates_leaf_failure_through_boolean_branches() {
    let eng = engine_with_corpus();
    let driver = EngineDriver::new(&eng, "notes", &[]);
    let tree = OperatorTree::Union(vec![
        OperatorTree::Term {
            query: "tokio".into(),
            field: Some("missing".into()),
            scoring: Some(TextScoringMode::BM25),
        },
        OperatorTree::Empty,
    ]);

    match driver.execute_node(&tree) {
        Err(SQLError::UnknownColumn(column)) => {
            assert_eq!(column, "missing");
        }
        other => panic!("expected the search helper error, got {other:?}"),
    }
}

#[test]
fn driver_rejects_declared_but_unindexed_text_column() {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE unindexed_notes (id INTEGER PRIMARY KEY, body TEXT)",
        &[],
    )
    .unwrap();
    eng.sql(
        "INSERT INTO unindexed_notes (id, body) VALUES (1, 'tokio')",
        &[],
    )
    .unwrap();
    let driver = EngineDriver::new(&eng, "unindexed_notes", &[]);

    let error = driver
        .execute_node(&OperatorTree::Term {
            query: "tokio".into(),
            field: Some("body".into()),
            scoring: Some(TextScoringMode::BM25),
        })
        .expect_err("an unindexed text column must not look like no matches");
    assert!(
        matches!(error, SQLError::TypeMismatch(ref message) if message.contains("has no text index")),
        "unexpected error: {error}"
    );
}

#[test]
fn schema_less_dynamic_table_keeps_registered_fts_fields_executable() {
    let eng = Engine::new();
    eng.create_default_table("dynamic_docs", vec!["body".into()])
        .unwrap();
    eng.add_document(
        "dynamic_docs",
        1,
        BTreeMap::from([("body".into(), Value::Str("dynamic token".into()))]),
    )
    .unwrap();
    let driver = EngineDriver::new(&eng, "dynamic_docs", &[]);
    let tree = OperatorTree::Term {
        query: "dynamic".into(),
        field: Some("body".into()),
        scoring: Some(TextScoringMode::BM25),
    };

    let result = driver.execute_node(&tree).unwrap();
    assert_eq!(
        result
            .as_posting()
            .unwrap()
            .entries()
            .iter()
            .map(|entry| entry.doc_id)
            .collect::<Vec<_>>(),
        vec![1]
    );
}

#[test]
fn graph_ir_node_executes_through_the_shared_driver() {
    let eng = engine_with_corpus();
    eng.create_graph("social").unwrap();
    eng.add_graph_vertex(Vertex::new(1, "Person"), "social")
        .unwrap();
    eng.add_graph_vertex(Vertex::new(2, "Person"), "social")
        .unwrap();
    eng.add_graph_edge(Edge::new(1, 1, 2, "follows"), "social")
        .unwrap();
    let driver = EngineDriver::new(&eng, "notes", &[]);

    let result = driver
        .execute_node(&OperatorTree::PageRank {
            graph: "social".into(),
        })
        .expect("PageRank must execute through EngineDriver");
    assert_eq!(
        result
            .as_posting()
            .expect("PageRank produces one posting per vertex")
            .doc_ids()
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
}

#[test]
fn malformed_empty_fusion_and_stage_nodes_return_typed_errors() {
    let engine = engine_with_corpus();
    let driver = EngineDriver::new(&engine, "notes", &[]);
    let malformed = [
        OperatorTree::ProbBoolFusion {
            signals: Vec::new(),
            mode: ProbBoolMode::And,
        },
        OperatorTree::LogOddsFusion {
            signals: Vec::new(),
            alpha: 0.5,
            gating: GatingSpec::Pass,
            weights: None,
            logit_min: None,
            logit_max: None,
            adaptive_weights: false,
        },
        OperatorTree::AttentionFusion {
            signals: Vec::new(),
            attention: Arc::new(AttentionFusion::new(0, 6, 0.5)),
            query_features: vec![0.0; 6],
        },
        OperatorTree::LearnedFusion {
            signals: Vec::new(),
            learned: Arc::new(LearnedFusion::new(0, 0.5)),
        },
        OperatorTree::MultiStage { stages: Vec::new() },
        OperatorTree::ProgressiveFusion {
            stages: Vec::new(),
            alpha: 0.5,
            gating: GatingSpec::Pass,
        },
    ];

    for tree in malformed {
        match driver.execute_node(&tree) {
            Err(SQLError::TypeMismatch(message)) => {
                assert!(
                    message.contains("requires at least one"),
                    "unexpected malformed-node error: {message}"
                );
            }
            other => panic!("expected typed malformed-node error, got {other:?}"),
        }
    }
}

#[test]
fn malformed_fusion_configuration_propagates_from_driver() {
    let engine = engine_with_corpus();
    let driver = EngineDriver::new(&engine, "notes", &[]);
    let log_odds_cases = [
        (
            OperatorTree::LogOddsFusion {
                signals: vec![OperatorTree::Empty],
                alpha: f64::NAN,
                gating: GatingSpec::Pass,
                weights: None,
                logit_min: None,
                logit_max: None,
                adaptive_weights: false,
            },
            "alpha",
        ),
        (
            OperatorTree::LogOddsFusion {
                signals: vec![OperatorTree::Empty],
                alpha: 0.5,
                gating: GatingSpec::Pass,
                weights: Some(vec![0.5, 0.5]),
                logit_min: None,
                logit_max: None,
                adaptive_weights: false,
            },
            "weights",
        ),
        (
            OperatorTree::LogOddsFusion {
                signals: vec![OperatorTree::Empty],
                alpha: 0.5,
                gating: GatingSpec::Pass,
                weights: None,
                logit_min: Some(vec![1.0]),
                logit_max: Some(vec![1.0]),
                adaptive_weights: false,
            },
            "bounds",
        ),
    ];
    for (tree, expected) in log_odds_cases {
        match driver.execute_node(&tree) {
            Err(SQLError::Internal(message)) => {
                assert!(message.contains(expected), "unexpected error: {message}");
            }
            other => panic!("expected typed log-odds error, got {other:?}"),
        }
    }

    let attention = OperatorTree::AttentionFusion {
        signals: vec![OperatorTree::Empty],
        attention: Arc::new(AttentionFusion::new(2, 6, 0.5)),
        query_features: vec![0.0; 6],
    };
    assert!(matches!(
        driver.execute_node(&attention),
        Err(SQLError::TypeMismatch(message)) if message.contains("signal count")
    ));

    let learned = OperatorTree::LearnedFusion {
        signals: vec![OperatorTree::Empty],
        learned: Arc::new(LearnedFusion::new(2, 0.5)),
    };
    assert!(matches!(
        driver.execute_node(&learned),
        Err(SQLError::TypeMismatch(message)) if message.contains("signal count")
    ));
}

#[test]
fn filter_rejects_candidate_missing_from_document_snapshot() {
    let engine = engine_with_corpus();
    let driver = EngineDriver::new(&engine, "notes", &[]);
    let tree = OperatorTree::Filter {
        field: "year".into(),
        // NotEquals deliberately bypasses the optional value-index fast path.
        predicate: Predicate::NotEquals(Value::Int(-1)),
        // Vertex aggregation over an empty source deterministically produces
        // synthetic document ID 0, which is absent from the table snapshot.
        source: Some(Box::new(OperatorTree::VertexAggregation {
            source: Box::new(OperatorTree::Empty),
            monoid: Arc::new(CountMonoid),
        })),
    };

    match driver.execute_node(&tree) {
        Err(SQLError::Internal(message)) => {
            assert!(
                message.contains("candidate 0"),
                "unexpected error: {message}"
            );
            assert!(message.contains("document-field snapshot"));
        }
        other => panic!("expected storage consistency error, got {other:?}"),
    }
}

#[test]
fn aggregation_errors_propagate_through_the_driver() {
    struct RejectingMonoid;

    impl AggregationMonoid for RejectingMonoid {
        fn identity(&self) -> AggState {
            AggState::Count(0)
        }

        fn accumulate(
            &self,
            _state: AggState,
            _value: &Value,
        ) -> uqa_storage::StorageBackendResult<AggState> {
            Err(uqa_storage::StorageBackendError::Other(
                "reject vertex value".into(),
            ))
        }

        fn combine(
            &self,
            left: AggState,
            _right: AggState,
        ) -> uqa_storage::StorageBackendResult<AggState> {
            Ok(left)
        }

        fn finalize(&self, _state: AggState) -> uqa_storage::StorageBackendResult<Value> {
            Ok(Value::Null)
        }
    }

    let engine = engine_with_corpus();
    let driver = EngineDriver::new(&engine, "notes", &[]);

    let sum_strings = OperatorTree::Aggregate {
        source: None,
        field: "title".into(),
        monoid: Arc::new(SumMonoid),
    };
    assert!(matches!(
        driver.execute_node(&sum_strings),
        Err(SQLError::Internal(message)) if message.contains("requires a numeric value")
    ));

    let vertex = OperatorTree::VertexAggregation {
        source: Box::new(OperatorTree::Term {
            query: "tokio".into(),
            field: Some("body".into()),
            scoring: Some(TextScoringMode::BM25),
        }),
        monoid: Arc::new(RejectingMonoid),
    };
    assert!(matches!(
        driver.execute_node(&vertex),
        Err(SQLError::Internal(message))
            if message.contains("VertexAggregation") && message.contains("reject vertex value")
    ));
}
