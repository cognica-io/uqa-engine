//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_engine::SQLParam;

#[test]
fn empty_phrase_graph_still_uses_current_session_budget_for_complete_analysis() {
    let directory = tempdir().unwrap();
    let engine = Engine::open(&directory.path().join("analysis.sqlite3")).unwrap();
    fixture(&engine);
    engine.register_named_analyzer("phrase_stop", r#"{"tokenizer":{"type":"whitespace"},"token_filters":[{"type":"stop","language":"none","custom_words":["red"]}]}"#).unwrap();
    engine
        .set_table_field_analyzer("phrases", "body", "phrase_stop", "search")
        .unwrap();
    engine
        .sql(
            "PREPARE ignored_phrase(text) AS SELECT id FROM phrases WHERE fts_match(body, $1)",
            &[],
        )
        .unwrap();
    let parameters = [SQLParam::Scalar(Value::Str(format!(
        "\"{}\"",
        "red ".repeat(4096)
    )))];
    for limit in ["64kB", "16MB", "64kB"] {
        engine
            .sql(&format!("SET work_mem = '{limit}'"), &[])
            .unwrap();
        let result = engine.sql("EXECUTE ignored_phrase($1)", &parameters);
        if limit == "64kB" {
            assert_eq!(result.unwrap_err().sqlstate(), Some("53200"));
        } else {
            assert!(result.unwrap().rows.is_empty());
        }
    }
    let observer = engine.new_session().unwrap();
    observer.sql("SET work_mem = '16MB'", &[]).unwrap();
    let sql = "SELECT id FROM phrases WHERE fts_match(body, $1)";
    assert!(observer.sql(sql, &parameters).unwrap().rows.is_empty());
    assert_eq!(
        engine.sql(sql, &parameters).unwrap_err().sqlstate(),
        Some("53200")
    );
    assert!(engine
        .sql("EXECUTE ignored_phrase('\"red\"')", &[])
        .unwrap()
        .rows
        .is_empty());
}

#[test]
fn phrase_provider_occurrence_reads_obey_session_work_mem() {
    let directory = tempdir().unwrap();
    let config = r#"{"tokenizer":{"type":"nori_tokenizer"},"token_filters":[]}"#;
    if let Err(error) = serde_json::from_str::<uqa_analysis::Analyzer>(config) {
        assert!(error
            .to_string()
            .contains("unknown variant `nori_tokenizer`"));
        return;
    }
    for provider in ["memory", "sqlite", "redb"] {
        let path = directory.path().join(provider);
        let engine = match provider {
            "memory" => Engine::new(),
            "sqlite" => Engine::open(&path).unwrap(),
            "redb" => redb(&path),
            _ => unreachable!(),
        };
        engine
            .sql(
                "CREATE TABLE nori_phrases (id INTEGER PRIMARY KEY, body TEXT); CREATE INDEX nori_phrases_fts ON nori_phrases USING gin(body)",
                &[],
            )
            .unwrap();
        engine
            .register_named_analyzer("nori_phrase", config)
            .unwrap();
        engine
            .set_table_field_analyzer("nori_phrases", "body", "nori_phrase", "both")
            .unwrap();
        let source = format!("{}서울", "서울 ".repeat(4096));
        engine
            .sql(
                "INSERT INTO nori_phrases (id, body) VALUES (1, $1)",
                &[SQLParam::Scalar(Value::Str(source))],
            )
            .unwrap();

        engine.sql("SET work_mem = '16MB'", &[]).unwrap();
        let query = r#"SELECT id FROM nori_phrases WHERE fts_match(body, '"서울 서울"')"#;
        let rows = engine.sql(query, &[]).unwrap().rows;
        assert!(
            rows.iter().any(|row| row["id"] == Value::Int(1)),
            "{provider}"
        );

        engine.sql("SET work_mem = '64kB'", &[]).unwrap();
        let error = engine.sql(query, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("53200"), "{provider}: {error}");

        engine.sql("SET work_mem = '16MB'", &[]).unwrap();
        let rows = engine.sql(query, &[]).unwrap().rows;
        assert!(
            rows.iter().any(|row| row["id"] == Value::Int(1)),
            "{provider}"
        );

        engine.cancel();
        let error = engine.sql(query, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("57014"), "{provider}: {error}");
        engine.reset_cancellation();
        let rows = engine.sql(query, &[]).unwrap().rows;
        assert!(
            rows.iter().any(|row| row["id"] == Value::Int(1)),
            "{provider}"
        );
    }
}
