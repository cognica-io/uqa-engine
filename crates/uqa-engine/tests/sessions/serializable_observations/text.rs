//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Text consumers retain logical term phantoms and semantic scoring statistics across provider sessions.

use super::{
    finish, fixtures, EngineDriver, OperatorOutput, OperatorTree, OperatorTreeDriver, Session,
};
use std::sync::Arc;
use uqa_execution::operator_tree::driver::context::RetrievalSnapshots;
use uqa_operators::{TextScoringMode, TextTopKPlan, TextTopKStrategy};
use uqa_scoring::{BM25Params, ScoringMode};
use uqa_storage::InvertedIndex;

#[path = "text/retained.rs"]
mod retained;

fn prepare(session: &Session) {
    session.sql("CREATE TABLE texts (id INTEGER PRIMARY KEY, body TEXT, other TEXT)");
    session.sql("CREATE INDEX texts_gin ON texts USING gin (body, other)");
    session
        .sql("INSERT INTO texts VALUES (1, 'alpha beta', 'heading'), (2, 'gamma delta', 'other')");
}

fn index(session: &Session) -> Arc<dyn InvertedIndex> {
    RetrievalSnapshots::snapshot_context(&session.engine, "texts")
        .unwrap()
        .unwrap()
        .inverted_index
        .unwrap()
}

fn pivot(a: &Session, b: &Session) {
    b.sql("SELECT v FROM right_t");
    a.sql("UPDATE right_t SET v = 2 WHERE id = 1");
}

fn search(session: &Session, route: &str, query: &str) -> usize {
    if route == "postings" {
        return index(session)
            .get_posting_list("body", query)
            .unwrap()
            .len();
    }
    if route == "sql" {
        return session
            .sql(&format!(
                "SELECT id FROM texts WHERE text_match(body, '{query}')"
            ))
            .rows
            .len();
    }
    let tree = if route == "attention" {
        // Fixed parameters isolate the attention feature read from calibration's independent corpus reads.
        OperatorTree::AttentionFusion {
            signals: [("body", "alpha"), ("other", "heading")]
                .into_iter()
                .map(|(field, query)| OperatorTree::Term {
                    query: query.into(),
                    field: Some(field.into()),
                    scoring: Some(TextScoringMode::CustomBayesianBM25(
                        uqa_scoring::BayesianBM25Params::default(),
                    )),
                    top_k: None,
                })
                .collect(),
            attention: Arc::new(uqa_fusion::AttentionFusion::new(2, 6, 0.5)),
            query_features: Vec::new(),
        }
    } else if route == "phrase" {
        OperatorTree::Phrase {
            query: query.into(),
            field: Some("body".into()),
            scoring: Some(TextScoringMode::BM25),
        }
    } else {
        OperatorTree::Term {
            query: query.into(),
            field: Some("body".into()),
            scoring: Some(TextScoringMode::BM25),
            top_k: match route {
                "exhaustive" => None,
                "wand" | "bmw" => Some(TextTopKPlan {
                    k: 1,
                    strategy: if route == "wand" {
                        TextTopKStrategy::Wand
                    } else {
                        TextTopKStrategy::BlockMaxWand
                    },
                }),
                _ => panic!("unknown text route"),
            },
        }
    };
    let OperatorOutput::Posting(rows) = EngineDriver::new(&session.engine, "texts", &[])
        .execute_node(&tree)
        .unwrap()
    else {
        panic!("expected postings");
    };
    rows.len()
}

#[test]
fn admitted_text_consumers_retain_absent_term_phantoms() {
    for route in ["postings", "sql", "exhaustive", "wand", "bmw", "phrase"] {
        let (_directory, sessions) = fixtures();
        for a in sessions {
            prepare(&a);
            if route == "bmw" {
                a.engine
                    .rebuild_text_block_max(
                        "texts",
                        "body",
                        &ScoringMode::BM25(BM25Params::default()),
                    )
                    .unwrap();
            }
            let b = a.sibling();
            a.begin();
            b.begin();
            assert_eq!(search(&a, route, "absent"), 0, "{route}");
            pivot(&a, &b);
            b.sql("INSERT INTO texts VALUES (3, 'absent', NULL), (4, 'unrelated', NULL)");
            finish(&a, &b, true);
            assert_eq!(search(&a, route, "absent"), 0);
        }
    }
}

#[test]
fn admitted_text_terms_and_scoring_statistics_have_distinct_write_footprints() {
    for (route, mutation, conflict) in [
        (
            "postings",
            "INSERT INTO texts VALUES (3, 'omega omega', NULL)",
            false,
        ),
        (
            "postings",
            "UPDATE texts SET body = 'gamma gamma' WHERE id = 2",
            false,
        ),
        (
            "sql",
            "UPDATE texts SET body = 'gamma gamma' WHERE id = 2",
            false,
        ),
        (
            "sql",
            "UPDATE texts SET body = 'gamma gamma gamma' WHERE id = 2",
            true,
        ),
        ("sql", "INSERT INTO texts VALUES (3, NULL, 'alpha')", false),
        ("sql", "INSERT INTO texts VALUES (3, '', NULL)", true),
        ("sql", "INSERT INTO texts VALUES (3, NULL, NULL)", false),
        ("postings", "DELETE FROM texts WHERE id = 1", true),
        (
            "postings",
            "UPDATE texts SET body = NULL WHERE id = 1",
            true,
        ),
        (
            "postings",
            "UPDATE texts SET body = 'alpha beta' WHERE id = 1",
            true,
        ),
        (
            "postings",
            "UPDATE texts SET body = 'omega omega' WHERE id = 1",
            true,
        ),
        (
            "postings",
            "INSERT INTO texts VALUES (3, 'alpha', NULL)",
            true,
        ),
        (
            "attention",
            "UPDATE texts SET body = 'gamma gamma' WHERE id = 2",
            true,
        ),
    ] {
        let (_directory, sessions) = fixtures();
        for a in sessions {
            prepare(&a);
            let b = a.sibling();
            a.begin();
            b.begin();
            assert_eq!(search(&a, route, "alpha"), 1, "{route}: {mutation}");
            pivot(&a, &b);
            b.sql(mutation);
            finish(&a, &b, conflict);
        }
    }
}

#[test]
fn admitted_parallel_calibration_retains_its_original_corpus_reads() {
    let (_directory, sessions) = fixtures();
    for a in sessions {
        prepare(&a);
        let b = a.sibling();
        a.begin();
        // An independently committed definition forces the fixed transaction's catalog refresh during later reads.
        b.sql("CREATE TABLE published_after_snapshot (v INTEGER)");
        b.begin();
        let rows = a.sql(
            "SELECT id, _score FROM texts WHERE fuse_attention(\
             bayesian_match(body, 'alpha'), bayesian_match(other, 'heading'))",
        );
        assert_eq!(rows.rows.len(), 1);
        assert_eq!(rows.rows[0]["id"], uqa_core::Value::Int(1));
        pivot(&a, &b);
        b.sql("UPDATE texts SET body = 'gamma gamma' WHERE id = 2");
        finish(&a, &b, true);
    }
}

#[test]
fn admitted_phrase_and_top_k_reads_include_occurrences_and_field_statistics() {
    for route in ["phrase", "wand", "bmw"] {
        for mutation in [
            "UPDATE texts SET body = 'beta alpha' WHERE id = 1",
            "UPDATE texts SET body = 'gamma gamma gamma' WHERE id = 2",
        ] {
            let (_directory, sessions) = fixtures();
            for a in sessions {
                prepare(&a);
                if route == "bmw" {
                    a.engine
                        .rebuild_text_block_max(
                            "texts",
                            "body",
                            &ScoringMode::BM25(BM25Params::default()),
                        )
                        .unwrap();
                }
                let b = a.sibling();
                a.begin();
                b.begin();
                assert_eq!(search(&a, route, "alpha beta"), 1);
                pivot(&a, &b);
                b.sql(mutation);
                finish(&a, &b, true);
            }
        }
    }
}

#[test]
fn admitted_unused_text_contexts_and_zero_limits_register_no_data_reads() {
    for route in ["context", "empty", "limit", "ordered", "aliased"] {
        let (_directory, sessions) = fixtures();
        for a in sessions {
            prepare(&a);
            let b = a.sibling();
            a.begin();
            b.begin();
            match route {
                "context" => {
                    index(&a).search_analyzer_revision("body").unwrap();
                }
                "empty" => {
                    assert_eq!(search(&a, "exhaustive", ""), 0);
                }
                _ => {
                    let query = match route {
                        "ordered" => "SELECT id FROM texts WHERE text_match(body, 'alpha') ORDER BY _score DESC, id LIMIT 0",
                        "aliased" => "SELECT t.id FROM texts AS t WHERE text_match(t.body, 'alpha') LIMIT 0",
                        _ => "SELECT id FROM texts WHERE text_match(body, 'alpha') LIMIT 0",
                    };
                    assert!(a.sql(query).rows.is_empty());
                }
            }
            pivot(&a, &b);
            b.sql("INSERT INTO texts VALUES (3, 'alpha', NULL)");
            finish(&a, &b, false);
        }
    }
}

#[test]
fn admitted_text_savepoint_undo_removes_intents_but_preserves_observed_dependencies() {
    for read_before_write in [false, true] {
        let (_directory, sessions) = fixtures();
        for a in sessions {
            prepare(&a);
            let b = a.sibling();
            a.begin();
            b.begin();
            if read_before_write {
                assert_eq!(search(&a, "postings", "alpha"), 1);
            }
            b.sql("SAVEPOINT text_write");
            b.sql("DELETE FROM texts WHERE id = 1");
            b.sql("ROLLBACK TO text_write");
            b.sql("RELEASE text_write");
            if !read_before_write {
                assert_eq!(search(&a, "postings", "alpha"), 1);
            }
            pivot(&a, &b);
            finish(&a, &b, read_before_write);
            assert_eq!(search(&a, "postings", "alpha"), 1);
        }
    }
}

#[test]
fn admitted_text_inventory_and_bulk_reads_keep_their_selected_data_scope() {
    for (route, mutation, conflict) in [
        (
            "bulk",
            "UPDATE texts SET body = 'gamma gamma' WHERE id = 2",
            false,
        ),
        (
            "bulk",
            "UPDATE texts SET body = 'beta alpha' WHERE id = 1",
            true,
        ),
        (
            "vocabulary",
            "UPDATE texts SET body = 'gamma gamma' WHERE id = 2",
            true,
        ),
        (
            "stats",
            "UPDATE texts SET body = 'gamma gamma' WHERE id = 2",
            true,
        ),
        ("count", "INSERT INTO texts VALUES (3, 'omega', NULL)", true),
        (
            "any_field",
            "UPDATE texts SET other = 'alpha' WHERE id = 2",
            true,
        ),
    ] {
        let (_directory, sessions) = fixtures();
        for a in sessions {
            prepare(&a);
            let b = a.sibling();
            a.begin();
            b.begin();
            match route {
                "bulk" => {
                    let inputs = index(&a)
                        .get_scoring_inputs_keys_bulk(
                            &[1],
                            "body",
                            &[uqa_storage::TokenTermKey::from_text("alpha")],
                        )
                        .unwrap();
                    assert_eq!(inputs, [(2, vec![1])]);
                }
                "vocabulary" => {
                    assert_eq!(index(&a).vocabulary_keys("body").unwrap().len(), 4);
                }
                "stats" => {
                    assert_eq!(a.engine.fts_index_stats(Some("texts")).unwrap().len(), 2);
                }
                "count" => {
                    assert_eq!(a.engine.document_count("texts").unwrap(), 2);
                }
                "any_field" => {
                    assert_eq!(
                        index(&a).get_posting_list_any_field("alpha").unwrap().len(),
                        1
                    );
                }
                _ => unreachable!(),
            }
            pivot(&a, &b);
            b.sql(mutation);
            finish(&a, &b, conflict);
        }
    }
}
