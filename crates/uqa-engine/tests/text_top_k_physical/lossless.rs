//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native Nori term identities through public ranking, calibration, SQL, and durable bounds.

use super::*;
use std::{path::Path, sync::Arc};

const CONFIG: &str = r#"{"tokenizer":{"type":"nori_tokenizer","decompound_mode":"mixed","discard_punctuation":false,"user_dictionary":"🙂a 가 나"},"token_filters":[]}"#;

fn available() -> bool {
    match serde_json::from_str::<uqa_analysis::Analyzer>(CONFIG) {
        Ok(_) => true,
        Err(error) => {
            assert!(error
                .to_string()
                .contains("unknown variant `nori_tokenizer`"));
            false
        }
    }
}

fn fixture(engine: &Engine) {
    engine.sql("CREATE TABLE docs(id INTEGER PRIMARY KEY, body TEXT); CREATE INDEX docs_fts ON docs USING gin(body)", &[]).unwrap();
    engine
        .register_named_analyzer("raw_korean", CONFIG)
        .unwrap();
    engine
        .set_table_field_analyzer("docs", "body", "raw_korean", "both")
        .unwrap();
    engine.sql("INSERT INTO docs VALUES (1, '🙂a'), (2, '🙂a 🙂a'), (3, '�'), (4, '서울'), (5, '🙂a 🙂a 🙂a')", &[]).unwrap();
}

fn verify(engine: &Engine, expected_algorithm: TextSearchAlgorithm) {
    let bm25 = ScoringMode::BM25(BM25Params::default());
    let single = engine
        .search("docs", "body", "🙂a", &bm25, usize::MAX)
        .unwrap();
    let mut ids = single.iter().map(|entry| entry.doc_id).collect::<Vec<_>>();
    ids.sort_unstable();
    assert_eq!(ids, [1, 2, 5]);
    let repeated = engine
        .search("docs", "body", "🙂a🙂a", &bm25, usize::MAX)
        .unwrap();
    for (one, twice) in single.iter().zip(&repeated) {
        assert_eq!(one.doc_id, twice.doc_id);
        assert!((2.0 * one.score - twice.score).abs() < 1e-12);
    }
    assert_eq!(
        engine
            .search("docs", "body", "�", &bm25, 10)
            .unwrap()
            .iter()
            .map(|entry| entry.doc_id)
            .collect::<Vec<_>>(),
        [3]
    );
    assert_top_k_matches_exhaustive(engine, "body", "🙂a🙂a", &bm25, 2, expected_algorithm);
    let sql = engine
        .sql(
            "SELECT id FROM docs WHERE text_match(body, '🙂a') ORDER BY id",
            &[],
        )
        .unwrap();
    assert_eq!(
        sql.rows
            .iter()
            .map(|row| row["id"].clone())
            .collect::<Vec<_>>(),
        [Value::Int(1), Value::Int(2), Value::Int(5)]
    );
    let boolean = engine
        .sql(
            "SELECT id FROM docs WHERE fts_match(body, '🙂a AND NOT �') ORDER BY id",
            &[],
        )
        .unwrap();
    assert_eq!(boolean.rows, sql.rows);
    let all_fields = engine
        .sql(
            "SELECT id FROM docs WHERE _all @@ '🙂a AND NOT �' ORDER BY id",
            &[],
        )
        .unwrap();
    assert_eq!(all_fields.rows, sql.rows);

    let attention = engine.sql("SELECT id, _score FROM docs WHERE fuse_attention(bayesian_match(body, '🙂a'), bayesian_match(body, '서울')) ORDER BY id", &[]).unwrap();
    assert_eq!(
        attention
            .rows
            .iter()
            .map(|row| row["id"].clone())
            .collect::<Vec<_>>(),
        [Value::Int(1), Value::Int(2), Value::Int(4), Value::Int(5)]
    );
    let bayesian = ScoringMode::BayesianBM25(BayesianBM25Params::default());
    assert_top_k_matches_exhaustive(engine, "body", "🙂a🙂a", &bayesian, 2, expected_algorithm);
    engine
        .estimate_scoring_params("docs", "body", 12, 1, 42)
        .unwrap();
    let report = engine
        .calibration_report("docs", "body", "🙂a", &[1, 1, 0, 0, 1])
        .unwrap();
    assert!(report.brier.is_finite());
    assert!(report.ece.is_finite());
}

#[test]
fn memory_sql_ranking_and_calibration_use_lossless_query_terms() {
    if !available() {
        return;
    }
    let engine = Engine::new();
    fixture(&engine);
    verify(&engine, TextSearchAlgorithm::Wand);
}

#[test]
fn sqlite_binary_block_bounds_survive_reopen_and_mutation() {
    if !available() {
        return;
    }
    let directory = tempdir().unwrap();
    let path = directory.path().join("raw-query.sqlite3");
    let engine = Engine::open(&path).unwrap();
    fixture(&engine);
    verify(&engine, TextSearchAlgorithm::Wand);
    assert!(engine
        .rebuild_text_block_max("docs", "body", &ScoringMode::default())
        .unwrap());
    drop(engine);
    let engine = Engine::open(&path).unwrap();
    verify(&engine, TextSearchAlgorithm::BlockMaxWand);
    engine
        .sql("UPDATE docs SET body = '🙂a 🙂a 🙂a' WHERE id = 1", &[])
        .unwrap();
    assert_top_k_matches_exhaustive(
        &engine,
        "body",
        "🙂a🙂a",
        &ScoringMode::default(),
        2,
        TextSearchAlgorithm::Wand,
    );
}

fn redb(path: &Path) -> Engine {
    Engine::from_persistent_provider(Arc::new(uqa_storage_redb::RedbStorage::open(path).unwrap()))
        .unwrap()
}

#[test]
fn redb_sql_ranking_and_calibration_retain_raw_terms_after_reopen() {
    if !available() {
        return;
    }
    let directory = tempdir().unwrap();
    let path = directory.path().join("raw-query.redb");
    let engine = redb(&path);
    fixture(&engine);
    verify(&engine, TextSearchAlgorithm::Wand);
    drop(engine);
    verify(&redb(&path), TextSearchAlgorithm::Wand);
}
