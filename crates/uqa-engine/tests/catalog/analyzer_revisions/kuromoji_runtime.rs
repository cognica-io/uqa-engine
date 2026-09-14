//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{execute, fixture, hits, Backend, TempDir};
use uqa_core::Value;
use uqa_engine::{Engine, SQLParam};

#[test]
fn kuromoji_sql_controls_preserve_errors_results_and_recovery() {
    if uqa_analysis::get_analyzer("kuromoji").is_err() {
        return;
    }
    for backend in [Backend::Memory, Backend::SQLite, Backend::Redb] {
        let directory = TempDir::new().unwrap();
        let engine = backend.open(&directory.path().join("controls.db"));
        let cancellation = engine.cancellation_token();
        engine
            .register_scalar_function("cancel_japanese_input", move |_: &[Value]| {
                cancellation.cancel();
                Ok(Value::Str("東京".into()))
            })
            .unwrap();
        for name in ["kuromoji", "kuromoji_completion"] {
            verify_memory(&engine, name);
            let error = engine
                .sql(
                    "SELECT * FROM analyze_text($1, cancel_japanese_input())",
                    &[SQLParam::Scalar(Value::Str(name.into()))],
                )
                .unwrap_err();
            assert_eq!(error.sqlstate(), Some("57014"), "{backend:?}/{name}");
            engine.reset_cancellation();
            assert_eq!(
                engine
                    .sql(
                        "SELECT * FROM analyze_text($1, '東京')",
                        &[SQLParam::Scalar(Value::Str(name.into()))]
                    )
                    .unwrap()
                    .rows
                    .len(),
                1
            );
        }
    }
}

fn verify_memory(engine: &Engine, name: &str) {
    let retained = engine
        .sql(
            "SELECT analysis FROM analyze_text($1, '東京')",
            &[SQLParam::Scalar(Value::Str(name.into()))],
        )
        .unwrap();
    let original = retained.rows[0]["analysis"].clone();
    let input = "東京 関西国際空港 ".repeat(256);
    let params = [Value::Str(name.into()), Value::Str(input)].map(SQLParam::Scalar);
    for sql in [
        "SELECT analysis FROM analyze_text($1, $2)",
        "SELECT uqa_highlight($2, '東京', NULL, NULL, NULL, NULL, $1) AS snippet",
    ] {
        execute(engine, "SET work_mem = '32kB'");
        assert_eq!(
            engine.sql(sql, &params).unwrap_err().sqlstate(),
            Some("53200"),
            "{name}: {sql}"
        );
        execute(engine, "SET work_mem = '16MB'");
        assert_eq!(engine.sql(sql, &params).unwrap().rows.len(), 1);
        assert_eq!(retained.rows[0]["analysis"], original);
    }
}

#[test]
fn kuromoji_bindings_refresh_sibling_sessions_after_commit_and_rollback() {
    if uqa_analysis::get_analyzer("kuromoji").is_err() {
        return;
    }
    for backend in [Backend::SQLite, Backend::Redb] {
        let directory = TempDir::new().unwrap();
        let engine = backend.open(&directory.path().join("sessions.db"));
        fixture(&engine);
        engine
            .set_table_field_analyzer("docs", "body", "keyword", "both")
            .unwrap();
        execute(&engine, "INSERT INTO docs VALUES (1, '東京大学')");
        let observer = engine.new_session().unwrap();
        assert!(hits(&observer, "docs", "body", "東京").is_empty());
        engine.begin().unwrap();
        engine
            .set_table_field_analyzer("docs", "body", "kuromoji", "both")
            .unwrap();
        assert_eq!(hits(&engine, "docs", "body", "東京"), [1]);
        assert!(hits(&observer, "docs", "body", "東京").is_empty());
        engine.rollback().unwrap();
        assert!(hits(&engine, "docs", "body", "東京").is_empty());
        engine
            .set_table_field_analyzer("docs", "body", "kuromoji", "both")
            .unwrap();
        assert_eq!(hits(&observer, "docs", "body", "東京大学"), [1]);
        let index = observer
            .get_table_analyzer("docs", "body", "index")
            .unwrap();
        engine.begin().unwrap();
        engine
            .set_table_field_analyzer("docs", "body", "keyword", "search")
            .unwrap();
        assert!(hits(&engine, "docs", "body", "東京大学").is_empty());
        engine.rollback().unwrap();
        assert_eq!(hits(&observer, "docs", "body", "東京大学"), [1]);
        engine
            .set_table_field_analyzer("docs", "body", "keyword", "search")
            .unwrap();
        assert!(hits(&observer, "docs", "body", "東京大学").is_empty());
        assert_eq!(
            observer
                .get_table_analyzer("docs", "body", "index")
                .unwrap(),
            index
        );
        assert_eq!(hits(&observer, "docs", "body", "東京"), [1]);
    }
}
