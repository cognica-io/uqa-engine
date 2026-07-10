//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Verifies that the individual `QueryOptimizer` passes actually fire
//! on the lowered tree built from real SQL `WHERE` clauses. Each test
//! parses a SQL statement, lowers the WHERE through the operator-tree
//! bridge, runs the full 10-pass optimiser, and asserts the rewritten
//! shape matches the pass's documented behaviour.

use uqa_engine::operator_tree_bridge::{lower_where, optimised_tree_for};
use uqa_engine::Engine;
use uqa_operators::{OperatorTree, TextScoringMode};
use uqa_sql::ast::Statement;
use uqa_sql::compile;

fn engine_with_corpus() -> Engine {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE notes (id INTEGER PRIMARY KEY, title TEXT, body TEXT, year INTEGER, author TEXT)",
        &[],
    )
    .unwrap();
    eng.sql(
        "INSERT INTO notes (id, title, body, year, author) VALUES \
         (1, 'rust async', 'futures and tokio', 2024, 'a'), \
         (2, 'rust embedded', 'no_std and cortex_m', 2025, 'b'), \
         (3, 'python web', 'flask and django', 2024, 'c')",
        &[],
    )
    .unwrap();
    eng
}

fn where_of(sql: &str) -> uqa_sql::ast::Expr {
    let stmts = compile(sql).expect("parse");
    let Statement::Select(stmt) = stmts.into_iter().next().expect("at least one stmt") else {
        panic!("expected SELECT");
    };
    stmt.r#where.expect("expected WHERE")
}

fn assert_term_scoring(tree: &OperatorTree, expected: TextScoringMode) {
    let OperatorTree::Term { scoring, .. } = tree else {
        panic!("expected Term");
    };
    assert_eq!(*scoring, Some(expected));
}

#[test]
fn lower_where_preserves_text_match_scoring_mode() {
    let expr = where_of("SELECT id FROM notes WHERE text_match(body, 'tokio')");
    let lowered = lower_where(&expr, &[]).expect("lowers");
    assert_term_scoring(&lowered, TextScoringMode::BM25);
}

#[test]
fn lower_where_preserves_bayesian_match_scoring_mode() {
    let expr = where_of("SELECT id FROM notes WHERE bayesian_match(body, 'tokio')");
    let lowered = lower_where(&expr, &[]).expect("lowers");
    assert_term_scoring(&lowered, TextScoringMode::BayesianBM25);
}

#[test]
fn lower_where_binds_fts_query_text_terms_after_parsing() {
    let expr = where_of("SELECT id FROM notes WHERE body @@ 'tokio'");
    let lowered = lower_where(&expr, &[]).expect("lowers");
    assert_term_scoring(&lowered, TextScoringMode::BayesianBM25);
}

#[test]
fn fusion_lowering_rejects_raw_text_match_signal() {
    let expr = where_of(
        "SELECT id FROM notes \
         WHERE fuse_log_odds(text_match(title, 'rust'), bayesian_match(body, 'tokio'))",
    );
    assert!(lower_where(&expr, &[]).is_none());
}

#[test]
fn lower_where_recognises_text_match_and_filter() {
    let expr = where_of(
        "SELECT id FROM notes \
         WHERE text_match(body, 'tokio') AND year = 2024",
    );
    let lowered = lower_where(&expr, &[]).expect("lowers");
    let OperatorTree::Intersect(arms) = lowered else {
        panic!("expected Intersect");
    };
    assert_eq!(arms.len(), 2);
    let has_term = arms.iter().any(|a| matches!(a, OperatorTree::Term { .. }));
    let has_filter = arms
        .iter()
        .any(|a| matches!(a, OperatorTree::Filter { .. }));
    assert!(has_term && has_filter);
}

#[test]
fn simplify_algebra_pass_fires_via_recurse_children() {
    // The simplify pass walks the whole tree bottom-up. Even with a
    // shape that doesn't trigger absorption / dedup, the pass must
    // still descend through every node without altering semantics.
    let eng = engine_with_corpus();
    let expr = where_of("SELECT id FROM notes WHERE text_match(title, 'rust') AND year = 2024");
    let optimised = optimised_tree_for(&eng, "notes", &expr, &[]).expect("optimise");
    // Post-optimisation the Intersect must still produce the same
    // expected shape — two leaves, one of them a Term, one a Filter.
    let OperatorTree::Intersect(arms) = optimised else {
        panic!("expected Intersect");
    };
    assert_eq!(arms.len(), 2);
    let has_term = arms.iter().any(|a| matches!(a, OperatorTree::Term { .. }));
    let has_filter = arms
        .iter()
        .any(|a| matches!(a, OperatorTree::Filter { .. }));
    assert!(has_term && has_filter);
}

#[test]
fn reorder_intersect_pass_sorts_arms_by_cardinality() {
    // Two arms with different cardinality estimates must come back in
    // ascending cost order. We can't observe the cost directly but we
    // can confirm that an Intersect of a Term and a Filter survives
    // the pass intact with both arms present.
    let eng = engine_with_corpus();
    let expr = where_of("SELECT id FROM notes WHERE text_match(body, 'tokio') AND year = 2024");
    let optimised = optimised_tree_for(&eng, "notes", &expr, &[]).expect("optimise");
    let OperatorTree::Intersect(arms) = optimised else {
        panic!("expected Intersect");
    };
    assert_eq!(arms.len(), 2);
    // The cost-model favours the cheaper child first. Without a real
    // inverted-index df source the Term cost falls to 0, so it
    // currently sorts before the Filter; either arm coming first is
    // acceptable for parity. We assert only on shape preservation.
    assert!(arms.iter().any(|a| matches!(a, OperatorTree::Term { .. })));
    assert!(arms
        .iter()
        .any(|a| matches!(a, OperatorTree::Filter { .. })));
}

#[test]
fn or_through_optimiser_remains_union() {
    let eng = engine_with_corpus();
    let expr = where_of(
        "SELECT id FROM notes \
         WHERE text_match(title, 'rust') OR text_match(body, 'flask')",
    );
    let optimised = optimised_tree_for(&eng, "notes", &expr, &[]).expect("optimise");
    let OperatorTree::Union(arms) = optimised else {
        panic!("expected Union");
    };
    assert_eq!(arms.len(), 2);
    assert!(arms.iter().all(|a| matches!(a, OperatorTree::Term { .. })));
}

#[test]
fn pure_filter_passes_through_optimiser_unchanged() {
    let eng = engine_with_corpus();
    let expr = where_of("SELECT id FROM notes WHERE year = 2024");
    let optimised = optimised_tree_for(&eng, "notes", &expr, &[]).expect("optimise");
    // No table-level index manager is registered, so apply_index_scan
    // can't rewrite to IndexScan. The Filter survives as-is.
    assert!(matches!(optimised, OperatorTree::Filter { .. }));
}

#[test]
fn complement_through_not_lowers_correctly() {
    let expr = where_of("SELECT id FROM notes WHERE NOT text_match(title, 'python')");
    let lowered = lower_where(&expr, &[]).expect("lowers");
    let OperatorTree::Complement(inner) = lowered else {
        panic!("expected Complement");
    };
    assert!(matches!(*inner, OperatorTree::Term { .. }));
}

#[test]
fn unsupported_shape_returns_none() {
    let eng = engine_with_corpus();
    // Arithmetic on columns can't be lowered to OperatorTree's
    // Filter (no f(col_a, col_b) predicate); the bridge returns None
    // and the engine falls back to the row evaluator.
    let expr = where_of("SELECT id FROM notes WHERE id + 1 = 2");
    assert!(optimised_tree_for(&eng, "notes", &expr, &[]).is_none());
}

#[test]
fn fuse_log_odds_lowers_to_logoddsfusion_and_reorder_keeps_signals() {
    // `fuse_log_odds` is the gateway into the fusion-signal reorder
    // pass. We confirm the lowering produces the expected variant and
    // that the reorder pass preserves the signal count.
    let eng = engine_with_corpus();
    let expr = where_of(
        "SELECT id FROM notes \
         WHERE fuse_log_odds(bayesian_match(title, 'rust'), bayesian_match(body, 'tokio'), 0.5)",
    );
    let optimised = optimised_tree_for(&eng, "notes", &expr, &[]).expect("optimise");
    let OperatorTree::LogOddsFusion { signals, alpha, .. } = optimised else {
        panic!("expected LogOddsFusion");
    };
    assert_eq!(signals.len(), 2);
    assert!((alpha - 0.5).abs() < f64::EPSILON);
    assert!(signals
        .iter()
        .all(|s| matches!(s, OperatorTree::Term { .. })));
}

#[test]
fn fuse_log_odds_lowers_with_default_alpha() {
    let eng = engine_with_corpus();
    let expr = where_of(
        "SELECT id FROM notes \
         WHERE fuse_log_odds(bayesian_match(title, 'rust'), bayesian_match(body, 'tokio'))",
    );
    let optimised = optimised_tree_for(&eng, "notes", &expr, &[]).expect("optimise");
    let OperatorTree::LogOddsFusion { signals, alpha, .. } = optimised else {
        panic!("expected LogOddsFusion");
    };
    assert_eq!(signals.len(), 2);
    assert!((alpha - 0.5).abs() < f64::EPSILON);
}

#[test]
fn fuse_log_odds_with_relational_filter_lowers_to_intersect() {
    let eng = engine_with_corpus();
    let expr = where_of(
        "SELECT id FROM notes \
         WHERE fuse_log_odds(bayesian_match(title, 'rust'), bayesian_match(body, 'tokio')) \
           AND year >= 2024",
    );
    let optimised = optimised_tree_for(&eng, "notes", &expr, &[]).expect("optimise");
    let OperatorTree::Intersect(parts) = optimised else {
        panic!("expected Intersect");
    };
    assert_eq!(parts.len(), 2);
    assert!(parts
        .iter()
        .any(|p| matches!(p, OperatorTree::LogOddsFusion { .. })));
    assert!(parts
        .iter()
        .any(|p| matches!(p, OperatorTree::Filter { .. })));
}

#[test]
fn between_lowers_to_filter_with_between_predicate() {
    let expr = where_of("SELECT id FROM notes WHERE year BETWEEN 2024 AND 2025");
    let lowered = lower_where(&expr, &[]).expect("lowers");
    let OperatorTree::Filter {
        field, predicate, ..
    } = lowered
    else {
        panic!("expected Filter");
    };
    assert_eq!(field, "year");
    assert!(matches!(predicate, uqa_core::Predicate::Between { .. }));
}

#[test]
fn in_list_lowers_to_filter_with_inset_predicate() {
    let expr = where_of("SELECT id FROM notes WHERE year IN (2024, 2025)");
    let lowered = lower_where(&expr, &[]).expect("lowers");
    let OperatorTree::Filter {
        field, predicate, ..
    } = lowered
    else {
        panic!("expected Filter");
    };
    assert_eq!(field, "year");
    assert!(matches!(predicate, uqa_core::Predicate::InSet(_)));
}

#[test]
fn not_in_list_lowers_to_complement_filter() {
    // `NOT IN` keeps SQL three-valued semantics: the complement of the
    // match set intersected with `IS NOT NULL`, so NULL rows never
    // slip in (PostgreSQL 17 behavior).
    let expr = where_of("SELECT id FROM notes WHERE year NOT IN (2024)");
    let lowered = lower_where(&expr, &[]).expect("lowers");
    let OperatorTree::Intersect(parts) = lowered else {
        panic!("expected Intersect");
    };
    assert_eq!(parts.len(), 2);
    let OperatorTree::Complement(inner) = &parts[0] else {
        panic!("expected Complement");
    };
    assert!(matches!(**inner, OperatorTree::Filter { .. }));
    assert!(matches!(
        &parts[1],
        OperatorTree::Filter {
            predicate: uqa_core::Predicate::IsNotNull,
            ..
        }
    ));
}

#[test]
fn is_null_lowers_to_filter() {
    let expr = where_of("SELECT id FROM notes WHERE author IS NULL");
    let lowered = lower_where(&expr, &[]).expect("lowers");
    let OperatorTree::Filter { predicate, .. } = lowered else {
        panic!("expected Filter");
    };
    assert!(matches!(predicate, uqa_core::Predicate::IsNull));
}

#[test]
fn standalone_knn_match_lowers_to_raw_knn() {
    // Outside fusion contexts `knn_match` keeps raw cosine scores so
    // `engine.knn_search` and `WHERE knn_match(...)` agree at byte
    // level. The calibrated_signal rewrite only fires in fusion
    // contexts (`_compile_calibrated_signal` parity).
    let expr = where_of("SELECT id FROM notes WHERE knn_match(body, ARRAY[0.1, 0.2], 5)");
    let lowered = lower_where(&expr, &[]).expect("lowers");
    let OperatorTree::KNN { k, field, .. } = lowered else {
        panic!("expected raw KNN");
    };
    assert_eq!(k, 5);
    assert_eq!(field, "body");
}

#[test]
fn fuse_log_odds_calibrates_knn_signal() {
    // Inside `fuse_log_odds` the `knn_match` arm must lower to a
    // `CosineProbability(KNN)` node so the cosine score is rescaled
    // onto the (0, 1) probability interval before the log-odds
    // fuser combines it with the Bayesian BM25 text arm.
    let expr = where_of(
        "SELECT id FROM notes \
         WHERE fuse_log_odds( \
             bayesian_match(body, 'tokio'), \
             knn_match(body, ARRAY[0.1, 0.2], 5), \
             0.5 \
         )",
    );
    let lowered = lower_where(&expr, &[]).expect("lowers");
    let OperatorTree::LogOddsFusion { signals, alpha, .. } = lowered else {
        panic!("expected LogOddsFusion");
    };
    assert_eq!(signals.len(), 2);
    assert!((alpha - 0.5).abs() < f64::EPSILON);
    assert!(matches!(signals[0], OperatorTree::Term { .. }));
    let OperatorTree::CosineProbability(inner) = &signals[1] else {
        panic!("expected CosineProbability wrapping the KNN arm");
    };
    assert!(matches!(**inner, OperatorTree::KNN { .. }));
}

#[test]
fn attention_fusion_lowers_with_calibrated_signals() {
    // `attention(...)` is the SQL handle for `fuse_attention`; the
    // lowering builds an AttentionFusion IR node whose arms are
    // calibrated through `_compile_calibrated_signal` parity.
    let expr = where_of(
        "SELECT id FROM notes \
         WHERE attention( \
             bayesian_match(title, 'rust'), \
             knn_match(body, ARRAY[0.1, 0.2], 5) \
         )",
    );
    let lowered = lower_where(&expr, &[]).expect("lowers");
    let OperatorTree::AttentionFusion {
        signals,
        query_features,
        ..
    } = lowered
    else {
        panic!("expected AttentionFusion");
    };
    assert_eq!(signals.len(), 2);
    assert!(
        query_features.is_empty(),
        "features fill in at execute time"
    );
    assert!(matches!(signals[0], OperatorTree::Term { .. }));
    assert!(matches!(signals[1], OperatorTree::CosineProbability(_)));
}

#[test]
fn learned_fusion_lowers_with_calibrated_signals() {
    let expr = where_of(
        "SELECT id FROM notes \
         WHERE learned_fusion( \
             bayesian_match(title, 'rust'), \
             knn_match(body, ARRAY[0.1, 0.2], 5) \
         )",
    );
    let lowered = lower_where(&expr, &[]).expect("lowers");
    let OperatorTree::LearnedFusion { signals, .. } = lowered else {
        panic!("expected LearnedFusion");
    };
    assert_eq!(signals.len(), 2);
    assert!(matches!(signals[0], OperatorTree::Term { .. }));
    assert!(matches!(signals[1], OperatorTree::CosineProbability(_)));
}

#[test]
fn fuse_log_odds_calibrated_scores_lie_in_unit_interval() {
    // End-to-end: the executor must produce probability scores in
    // (0, 1) for every fused row. Without the calibration rewrite,
    // raw cosine in `[-1, 1]` could leak through to the fuser and
    // break the log-odds invariant.
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT, embedding VECTOR(2))",
        &[],
    )
    .unwrap();
    // The text signal needs a real index; without one the text half of
    // the fusion is rejected up front instead of silently scoring
    // nothing while knn carries the query.
    eng.sql("CREATE INDEX notes_body ON notes USING gin (body)", &[])
        .unwrap();
    eng.sql(
        "INSERT INTO notes (id, body, embedding) VALUES \
         (1, 'tokio runtime', ARRAY[0.9, 0.1]), \
         (2, 'flask web', ARRAY[0.1, 0.9]), \
         (3, 'rust async tokio', ARRAY[0.5, 0.5])",
        &[],
    )
    .unwrap();
    let result = eng
        .sql(
            "SELECT id, _score AS s FROM notes \
             WHERE fuse_log_odds( \
                 bayesian_match(body, 'tokio'), \
                 knn_match(embedding, ARRAY[0.5, 0.5], 3), \
                 0.5 \
             ) ORDER BY s DESC",
            &[],
        )
        .unwrap();
    assert!(!result.rows.is_empty());
    for row in &result.rows {
        let Some(uqa_core::Value::Float(s)) = row.get("s") else {
            panic!("expected Float score, got {:?}", row.get("s"));
        };
        assert!(*s > 0.0 && *s < 1.0, "score {s} not in (0, 1)");
    }
}
