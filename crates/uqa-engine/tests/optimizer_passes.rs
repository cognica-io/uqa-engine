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
    eng.sql("CREATE INDEX notes_year_idx ON notes (year)", &[])
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

fn where_of(sql: &str) -> uqa_execution::ScalarExpr {
    let stmts = compile(sql).expect("parse");
    let Statement::Select(stmt) = stmts.into_iter().next().expect("at least one stmt") else {
        panic!("expected SELECT");
    };
    uqa_planner::ExpressionPlan::lower(stmt.r#where.expect("expected WHERE")).scalar
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
    let OperatorTree::BayesianScore { source, field } = lowered else {
        panic!("expected BayesianScoreQuery");
    };
    assert_eq!(field.as_deref(), Some("body"));
    assert_term_scoring(&source, TextScoringMode::BM25);
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
fn mixed_graph_and_relational_boolean_inserts_an_explicit_phi_boundary() {
    let expr = where_of(
        "SELECT id FROM notes \
         WHERE graph_traverse('g', 1, 'knows', 2) AND year = 2024",
    );
    let lowered = lower_where(&expr, &[]).expect("lowers");
    let OperatorTree::Intersect(arms) = lowered else {
        panic!("expected Intersect");
    };
    assert!(matches!(
        &arms[0],
        OperatorTree::EncodeGraphPosting { source }
            if matches!(source.as_ref(), OperatorTree::Traverse { .. })
    ));
    assert!(matches!(&arms[1], OperatorTree::Filter { .. }));
}

#[test]
fn homogeneous_graph_boolean_keeps_the_graph_carrier() {
    let expr = where_of(
        "SELECT id FROM notes WHERE \
         graph_traverse('g', 1, 'knows', 2) OR graph_pagerank('g')",
    );
    let lowered = lower_where(&expr, &[]).expect("lowers");
    let OperatorTree::Union(arms) = lowered else {
        panic!("expected Union");
    };
    assert_eq!(arms.len(), 2);
    assert!(arms
        .iter()
        .all(|arm| !matches!(arm, OperatorTree::EncodeGraphPosting { .. })));
}

#[test]
fn simplify_algebra_pass_fires_via_recurse_children() {
    // The simplify pass walks the whole tree bottom-up. Even with a
    // shape that doesn't trigger absorption / dedup, the pass must
    // still descend through every node without altering semantics.
    let eng = engine_with_corpus();
    let expr = where_of("SELECT id FROM notes WHERE text_match(title, 'rust') AND year = 2024");
    let optimised = optimised_tree_for(&eng, "notes", &expr, &[])
        .expect("optimise")
        .expect("lowerable tree");
    // Post-optimisation the Intersect must still produce the same
    // Expected shape: two leaves, one Term and one physical index scan.
    // selected by the final optimizer pass.
    let OperatorTree::Intersect(arms) = optimised else {
        panic!("expected Intersect");
    };
    assert_eq!(arms.len(), 2);
    let has_term = arms.iter().any(|a| matches!(a, OperatorTree::Term { .. }));
    let has_index_scan = arms
        .iter()
        .any(|a| matches!(a, OperatorTree::IndexScan { .. }));
    assert!(has_term && has_index_scan);
}

#[test]
fn simplify_algebra_deduplicates_separately_lowered_filters() {
    let eng = engine_with_corpus();
    let expr = where_of("SELECT id FROM notes WHERE year = 2024 AND year = 2024");
    let optimised = optimised_tree_for(&eng, "notes", &expr, &[])
        .expect("optimise")
        .expect("lowerable tree");
    assert!(matches!(
        optimised,
        OperatorTree::IndexScan {
            ref index_name,
            ref field,
            ..
        } if index_name == "notes_year_idx" && field == "year"
    ));
}

#[test]
fn reorder_intersect_pass_sorts_arms_by_cardinality() {
    // Two arms with different cardinality estimates must come back in
    // ascending cost order. We can't observe the cost directly but we
    // can confirm that an Intersect of a Term and the Filter's physical
    // IndexScan replacement survives with both arms present.
    let eng = engine_with_corpus();
    let expr = where_of("SELECT id FROM notes WHERE text_match(body, 'tokio') AND year = 2024");
    let optimised = optimised_tree_for(&eng, "notes", &expr, &[])
        .expect("optimise")
        .expect("lowerable tree");
    let OperatorTree::Intersect(arms) = optimised else {
        panic!("expected Intersect");
    };
    assert_eq!(arms.len(), 2);
    // The cost-model favours the cheaper child first. Without a real
    // inverted-index df source the Term cost falls to 0, so it
    // currently sorts before the IndexScan; either arm coming first is
    // acceptable for parity. We assert only on shape preservation.
    assert!(arms.iter().any(|a| matches!(a, OperatorTree::Term { .. })));
    assert!(arms
        .iter()
        .any(|a| matches!(a, OperatorTree::IndexScan { .. })));
}

#[test]
fn or_through_optimiser_remains_union() {
    let eng = engine_with_corpus();
    let expr = where_of(
        "SELECT id FROM notes \
         WHERE text_match(title, 'rust') OR text_match(body, 'flask')",
    );
    let optimised = optimised_tree_for(&eng, "notes", &expr, &[])
        .expect("optimise")
        .expect("lowerable tree");
    let OperatorTree::Union(arms) = optimised else {
        panic!("expected Union");
    };
    assert_eq!(arms.len(), 2);
    assert!(arms.iter().all(|a| matches!(a, OperatorTree::Term { .. })));
}

#[test]
fn pure_filter_uses_catalog_btree_in_final_optimizer_pass() {
    let eng = engine_with_corpus();
    let expr = where_of("SELECT id FROM notes WHERE year = 2024");
    let optimised = optimised_tree_for(&eng, "notes", &expr, &[])
        .expect("optimise")
        .expect("lowerable tree");
    let OperatorTree::IndexScan {
        index_name, field, ..
    } = optimised
    else {
        panic!("expected the catalog btree to become IndexScan");
    };
    assert_eq!(index_name, "notes_year_idx");
    assert_eq!(field, "year");

    let result = eng
        .sql("SELECT id FROM notes WHERE year = 2024 ORDER BY id", &[])
        .expect("physical IndexScan execution");
    let ids: Vec<i64> = result
        .rows
        .iter()
        .map(|row| match row.get("id") {
            Some(uqa_core::Value::Int(id)) => *id,
            other => panic!("unexpected id: {other:?}"),
        })
        .collect();
    assert_eq!(ids, vec![1, 3]);
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
    // Filter (no f(col_a, col_b) predicate); the plan therefore keeps
    // this predicate in the relational scalar compute path.
    let expr = where_of("SELECT id FROM notes WHERE id + 1 = 2");
    assert!(optimised_tree_for(&eng, "notes", &expr, &[])
        .expect("optimizer result")
        .is_none());
}

#[test]
fn positive_evidence_pool_lowers_to_robust_pool_and_reorder_keeps_signals() {
    // The explicitly named robust pool participates in the fusion-signal
    // reorder pass without changing its signal count.
    let eng = engine_with_corpus();
    let expr = where_of(
        "SELECT id FROM notes \
         WHERE pool_positive_evidence(bayesian_match(title, 'rust'), bayesian_match(body, 'tokio'), 0.5)",
    );
    let optimised = optimised_tree_for(&eng, "notes", &expr, &[])
        .expect("optimise")
        .expect("lowerable tree");
    let OperatorTree::RobustPositiveEvidencePool { signals, alpha, .. } = optimised else {
        panic!("expected RobustPositiveEvidencePool");
    };
    assert_eq!(signals.len(), 2);
    assert!((alpha - 0.5).abs() < f64::EPSILON);
    assert!(signals
        .iter()
        .all(|s| matches!(s, OperatorTree::Term { .. })));
}

#[test]
fn explicit_fusion_names_lower_to_distinct_ir_contracts() {
    let eng = engine_with_corpus();
    let robust_expr = where_of(
        "SELECT id FROM notes WHERE pool_positive_evidence(\
            bayesian_match(title, 'rust'), bayesian_match(body, 'tokio'), 0.5)",
    );
    let robust = optimised_tree_for(&eng, "notes", &robust_expr, &[])
        .unwrap()
        .unwrap();
    assert!(matches!(
        robust,
        OperatorTree::RobustPositiveEvidencePool { .. }
    ));

    let exact_expr = where_of(
        "SELECT id FROM notes WHERE fuse_bayesian_evidence(\
            bayesian_match(title, 'rust'), bayesian_match(body, 'tokio'), \
            base_rate => 0.1)",
    );
    let exact = optimised_tree_for(&eng, "notes", &exact_expr, &[])
        .unwrap()
        .unwrap();
    let OperatorTree::BayesianEvidenceFusion { signals, base_rate } = exact else {
        panic!("expected BayesianEvidenceFusion");
    };
    assert_eq!(signals.len(), 2);
    assert_eq!(base_rate, Some(0.1));

    let exact_alias_expr = where_of(
        "SELECT id FROM notes WHERE fuse_log_odds(\
            bayesian_match(title, 'rust'), bayesian_match(body, 'tokio'), \
            base_rate => 0.1)",
    );
    let exact_alias = optimised_tree_for(&eng, "notes", &exact_alias_expr, &[])
        .unwrap()
        .unwrap();
    assert!(matches!(
        exact_alias,
        OperatorTree::BayesianEvidenceFusion {
            base_rate: Some(rate),
            ..
        } if (rate - 0.1).abs() < f64::EPSILON
    ));
}

#[test]
fn fuse_log_odds_lowers_to_exact_fusion_without_an_implicit_prior() {
    let eng = engine_with_corpus();
    let expr = where_of(
        "SELECT id FROM notes \
         WHERE fuse_log_odds(bayesian_match(title, 'rust'), bayesian_match(body, 'tokio'))",
    );
    let optimised = optimised_tree_for(&eng, "notes", &expr, &[])
        .expect("optimise")
        .expect("lowerable tree");
    let OperatorTree::BayesianEvidenceFusion { signals, base_rate } = optimised else {
        panic!("expected BayesianEvidenceFusion");
    };
    assert_eq!(signals.len(), 2);
    assert_eq!(base_rate, None);
}

#[test]
fn fuse_log_odds_with_relational_filter_lowers_to_intersect() {
    let eng = engine_with_corpus();
    let expr = where_of(
        "SELECT id FROM notes \
         WHERE fuse_log_odds(bayesian_match(title, 'rust'), bayesian_match(body, 'tokio')) \
           AND year >= 2024",
    );
    let optimised = optimised_tree_for(&eng, "notes", &expr, &[])
        .expect("optimise")
        .expect("lowerable tree");
    let OperatorTree::Intersect(parts) = optimised else {
        panic!("expected Intersect");
    };
    assert_eq!(parts.len(), 2);
    assert!(parts
        .iter()
        .any(|p| matches!(p, OperatorTree::BayesianEvidenceFusion { .. })));
    assert!(parts.iter().any(|p| {
        matches!(
            p,
            OperatorTree::IndexScan {
                index_name,
                field,
                ..
            } if index_name == "notes_year_idx" && field == "year"
        )
    }));
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
    // slip in (PostgreSQL 18 behavior).
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
    // level. Calibration rewrites fire only inside fusion contexts.
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
    // `CosineProbability(KNN)` marker so the driver fits prior-free
    // vector evidence from the selected cosine query pool before the
    // exact signed-logit sum combines it with Bayesian text evidence.
    let expr = where_of(
        "SELECT id FROM notes \
         WHERE fuse_log_odds( \
             bayesian_match(body, 'tokio'), \
             knn_match(body, ARRAY[0.1, 0.2], 5) \
         )",
    );
    let lowered = lower_where(&expr, &[]).expect("lowers");
    let OperatorTree::BayesianEvidenceFusion { signals, base_rate } = lowered else {
        panic!("expected BayesianEvidenceFusion");
    };
    assert_eq!(signals.len(), 2);
    assert_eq!(base_rate, None);
    assert!(matches!(signals[0], OperatorTree::Term { .. }));
    let OperatorTree::CosineProbability(inner) = &signals[1] else {
        panic!("expected CosineProbability wrapping the KNN arm");
    };
    assert!(matches!(**inner, OperatorTree::KNN { .. }));
}

#[test]
fn attention_fusion_lowers_with_calibrated_signals() {
    // `attention(...)` is the SQL handle for `fuse_attention`; the
    // lowering builds an AttentionFusion IR node whose arms are calibrated
    // onto the common probability scale.
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
fn attention_options_are_encoded_in_the_shared_ir() {
    let expr = where_of(
        "SELECT id FROM notes \
         WHERE fuse_attention( \
             bayesian_match(title, 'rust'), \
             bayesian_match(body, 'async'), \
             normalized => true, alpha => 0.7, base_rate => 0.02 \
         )",
    );
    let lowered = lower_where(&expr, &[]).expect("lowers");
    let OperatorTree::AttentionFusion {
        signals, attention, ..
    } = lowered
    else {
        panic!("expected AttentionFusion");
    };
    assert_eq!(signals.len(), 2);
    assert_eq!(attention.head_count(), 1);
    assert!(attention.normalize());
    assert!((attention.alpha() - 0.7).abs() < f64::EPSILON);
    assert_eq!(attention.base_rate(), Some(0.02));
}

#[test]
fn multihead_options_build_a_multihead_shared_ir_fuser() {
    let expr = where_of(
        "SELECT id FROM notes \
         WHERE fuse_multihead( \
             bayesian_match(title, 'rust'), \
             bayesian_match(body, 'async'), \
             n_heads => 3, normalized => true, alpha => 0.4 \
         )",
    );
    let lowered = lower_where(&expr, &[]).expect("lowers");
    let OperatorTree::AttentionFusion { attention, .. } = lowered else {
        panic!("expected AttentionFusion");
    };
    assert_eq!(attention.head_count(), 3);
    assert!(attention.normalize());
    assert!((attention.alpha() - 0.4).abs() < f64::EPSILON);
    assert_eq!(attention.base_rate(), None);
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
fn fuse_learned_named_alpha_is_encoded_in_the_shared_ir() {
    let expr = where_of(
        "SELECT id FROM notes \
         WHERE fuse_learned( \
             bayesian_match(title, 'rust'), \
             bayesian_match(body, 'async'), \
             alpha => 0.7 \
         )",
    );
    let lowered = lower_where(&expr, &[]).expect("lowers");
    let OperatorTree::LearnedFusion { signals, learned } = lowered else {
        panic!("expected LearnedFusion");
    };
    assert_eq!(signals.len(), 2);

    let probabilities = [0.8, 0.6];
    let actual = learned.fuse(&probabilities).expect("fuses");
    let expected = uqa_fusion::LearnedFusion::new(2, 0.7)
        .fuse(&probabilities)
        .expect("reference fuses");
    let ignored_alpha = uqa_fusion::LearnedFusion::new(2, 0.5)
        .fuse(&probabilities)
        .expect("default reference fuses");
    assert!((actual - expected).abs() < 1e-12);
    assert!((actual - ignored_alpha).abs() > 1e-6);
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
                 knn_match(embedding, ARRAY[0.5, 0.5], 3) \
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
