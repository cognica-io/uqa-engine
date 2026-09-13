//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL phrase graphs through retained revisions, ranking, transactions, and durable providers.

use super::*;
use std::{path::Path, sync::Arc};

#[path = "phrases/runtime.rs"]
mod runtime;

fn ids(engine: &Engine, predicate: &str) -> Vec<Value> {
    engine
        .sql(
            &format!("SELECT id FROM phrases WHERE {predicate} ORDER BY id"),
            &[],
        )
        .unwrap()
        .rows
        .into_iter()
        .map(|mut row| row.remove("id").unwrap())
        .collect()
}

fn fixture(engine: &Engine) {
    engine.sql("CREATE TABLE phrases(id INTEGER PRIMARY KEY, body TEXT, title TEXT); CREATE INDEX phrases_body ON phrases USING gin(body); CREATE INDEX phrases_title ON phrases USING gin(title)", &[]).unwrap();
    engine
        .register_named_analyzer(
            "phrase_words",
            r#"{"tokenizer":{"type":"whitespace"},"token_filters":[]}"#,
        )
        .unwrap();
    for field in ["body", "title"] {
        engine
            .set_table_field_analyzer("phrases", field, "phrase_words", "both")
            .unwrap();
    }
    engine.sql("INSERT INTO phrases VALUES (1, 'red fox', ''), (2, 'fox red', ''), (3, 'red quick fox', ''), (4, 'red red fox', ''), (5, 'red', 'fox'), (6, '', 'red fox'), (7, 'fox fox fox red red red', '')", &[]).unwrap();
}

fn verify(engine: &Engine) {
    assert_eq!(
        ids(engine, r#"fts_match(body, '"red fox"')"#),
        [Value::Int(1), Value::Int(4)]
    );
    assert_eq!(
        ids(engine, r#"fts_match(body, '"red red fox"')"#),
        [Value::Int(4)]
    );
    assert_eq!(
        ids(engine, r#"fts_match('_all', '"red fox"')"#),
        [Value::Int(1), Value::Int(4), Value::Int(6)]
    );
    assert_eq!(
        ids(engine, r#"fts_match(body, 'red AND NOT "red fox"')"#),
        [Value::Int(2), Value::Int(3), Value::Int(5), Value::Int(7)]
    );
    assert_eq!(ids(engine, "text_match(body, 'red fox')").len(), 6);
    let ranked = engine.sql(r#"SELECT id, _score FROM phrases WHERE fts_match(body, '"red fox"') ORDER BY _score DESC, id LIMIT 1"#, &[]).unwrap();
    assert_eq!(ranked.rows.len(), 1);
    assert!([Value::Int(1), Value::Int(4)].contains(&ranked.rows[0]["id"]));
    engine
        .sql(
            "BEGIN; UPDATE phrases SET body = 'red fox' WHERE id = 2",
            &[],
        )
        .unwrap();
    assert_eq!(
        ids(engine, r#"fts_match(body, '"red fox"')"#),
        [Value::Int(1), Value::Int(2), Value::Int(4)]
    );
    engine.sql("ROLLBACK", &[]).unwrap();
    assert_eq!(
        ids(engine, r#"fts_match(body, '"red fox"')"#),
        [Value::Int(1), Value::Int(4)]
    );
}

#[test]
fn memory_phrase_support_precedes_ranking_and_keeps_fields_separate() {
    let engine = Engine::new();
    fixture(&engine);
    verify(&engine);
}

#[test]
fn sqlite_phrase_occurrences_survive_reopen_and_rollback() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("phrases.sqlite3");
    let engine = Engine::open(&path).unwrap();
    fixture(&engine);
    drop(engine);
    verify(&Engine::open(&path).unwrap());
}

fn redb(path: &Path) -> Engine {
    Engine::from_persistent_provider(Arc::new(uqa_storage_redb::RedbStorage::open(path).unwrap()))
        .unwrap()
}

#[test]
fn redb_phrase_occurrences_survive_reopen_and_rollback() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("phrases.redb");
    let engine = redb(&path);
    fixture(&engine);
    drop(engine);
    verify(&redb(&path));
}

#[test]
fn search_revision_controls_complete_phrase_analysis() {
    let engine = Engine::new();
    fixture(&engine);
    engine
        .register_named_analyzer(
            "phrase_keyword",
            r#"{"tokenizer":{"type":"keyword"},"token_filters":[]}"#,
        )
        .unwrap();
    engine
        .set_table_field_analyzer("phrases", "body", "phrase_keyword", "search")
        .unwrap();
    assert!(ids(&engine, r#"fts_match(body, '"red fox"')"#).is_empty());
    engine
        .set_table_field_analyzer("phrases", "body", "phrase_words", "search")
        .unwrap();
    assert_eq!(
        ids(&engine, r#"fts_match(body, '"red fox"')"#),
        [Value::Int(1), Value::Int(4)]
    );
}

#[test]
fn phrase_calibration_uses_all_emitted_query_occurrences() {
    let engine = Engine::new();
    fixture(&engine);
    let params = engine.bayesian_params_for("phrases", "body").unwrap();
    let raw = engine
        .search(
            "phrases",
            "body",
            "red red fox",
            &ScoringMode::BM25(BM25Params::default()),
            usize::MAX,
        )
        .unwrap();
    let expected_raw = raw.iter().find(|entry| entry.doc_id == 4).unwrap().score;
    let params = params.scaled_for_query_terms(3);
    let expected = uqa_scoring::sigmoid(params.alpha * (expected_raw - params.beta));
    let rows = engine
        .sql(
            r#"SELECT id, _score FROM phrases WHERE fts_match(body, '"red red fox"')"#,
            &[],
        )
        .unwrap()
        .rows;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["id"], Value::Int(4));
    let Value::Float(score) = rows[0]["_score"] else {
        panic!("expected phrase score")
    };
    assert!((score - expected).abs() < 1e-12, "{score} != {expected}");
}

#[test]
fn phrase_memory_limit_is_a_recoverable_sql_error() {
    let engine = Engine::new();
    fixture(&engine);
    engine.sql("SET work_mem = '64kB'", &[]).unwrap();
    let phrase = "red ".repeat(4096);
    let error = engine
        .sql(
            &format!("SELECT id FROM phrases WHERE fts_match(body, '\"{phrase}\"')"),
            &[],
        )
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("53200"));
    assert_eq!(
        ids(&engine, r#"fts_match(body, '"red fox"')"#),
        [Value::Int(1), Value::Int(4)]
    );
}

#[test]
fn nori_graph_paths_and_raw_terms_survive_provider_reopen() {
    let config = r#"{"tokenizer":{"type":"nori_tokenizer","decompound_mode":"mixed","user_dictionary":"서울역 서울 역\n🙂a 가 나\nc++"},"token_filters":[]}"#;
    if let Err(error) = serde_json::from_str::<uqa_analysis::Analyzer>(config) {
        assert!(error
            .to_string()
            .contains("unknown variant `nori_tokenizer`"));
        return;
    }
    let directory = tempdir().unwrap();
    for provider in ["memory", "sqlite", "redb"] {
        let path = directory.path().join(provider);
        let open = || match provider {
            "memory" => Engine::new(),
            "sqlite" => Engine::open(&path).unwrap(),
            "redb" => redb(&path),
            _ => unreachable!(),
        };
        let engine = open();
        engine.sql("CREATE TABLE phrases(id INTEGER PRIMARY KEY, body TEXT); CREATE INDEX phrases_fts ON phrases USING gin(body)", &[]).unwrap();
        engine
            .register_named_analyzer("phrase_nori", config)
            .unwrap();
        engine
            .set_table_field_analyzer("phrases", "body", "phrase_nori", "both")
            .unwrap();
        engine.sql("INSERT INTO phrases VALUES (1, '서울역'), (2, '서울 역'), (3, '역 서울'), (4, '서울 부산 역'), (5, '🙂a c++'), (6, 'c++ 🙂a'), (7, '� c++')", &[]).unwrap();
        let engine = if provider == "memory" {
            engine
        } else {
            drop(engine);
            open()
        };
        assert_eq!(
            ids(&engine, r#"fts_match(body, '"서울 역"')"#),
            [Value::Int(1), Value::Int(2)],
            "{provider}"
        );
        assert_eq!(
            ids(&engine, r#"fts_match(body, '"서울역"')"#),
            [Value::Int(1), Value::Int(2)],
            "{provider}"
        );
        assert_eq!(
            ids(&engine, r#"fts_match(body, '"🙂a c++"')"#),
            [Value::Int(5)],
            "{provider}"
        );
    }
}
