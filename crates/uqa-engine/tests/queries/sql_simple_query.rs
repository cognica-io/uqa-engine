//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Real execution of `PostgreSQL` command completions and simple-query boundaries.

use uqa_engine::Engine;
use uqa_sql::SQLError;

fn verify_command_completion_oracle(engine: &Engine) {
    let oracle: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../tests/parity/pg18/command_completion_oracle.expected.json"
    ))
    .unwrap();
    assert!(oracle["postgresql_version"]
        .as_str()
        .unwrap()
        .starts_with("PostgreSQL 18.4"));
    for case in oracle["cases"].as_array().unwrap() {
        let sql = case["sql"].as_str().unwrap();
        let mut completions = Vec::new();
        let outcome = engine.sql_simple_query(sql, &[], |result| {
            completions.push(result.command_tag.clone());
            Ok(())
        });
        assert_eq!(
            serde_json::to_value(completions).unwrap(),
            case["command_tags"],
            "{sql}: {outcome:?}"
        );
        let error = outcome.err().map(
            |error| serde_json::json!({"sqlstate": error.sqlstate(), "message": error.to_string()}),
        );
        assert_eq!(serde_json::to_value(error).unwrap(), case["error"], "{sql}");
    }
}

#[test]
fn simple_query_completions_match_postgresql_memory() {
    verify_command_completion_oracle(&Engine::new());
}

#[test]
fn simple_query_completions_match_postgresql_sqlite() {
    let directory = tempfile::tempdir().unwrap();
    let engine = Engine::open(&directory.path().join("completion.uqa")).unwrap();
    verify_command_completion_oracle(&engine);
}

#[test]
fn direct_sql_completion_survives_statement_and_plan_caches() {
    let engine = Engine::new();
    for _ in 0..3 {
        assert_eq!(
            engine
                .sql("SELECT 1 AS value", &[])
                .unwrap()
                .command_tag
                .as_deref(),
            Some("SELECT 1")
        );
    }
    engine
        .sql("CREATE TABLE values_log (value integer)", &[])
        .unwrap();
    for _ in 0..3 {
        assert_eq!(
            engine
                .sql("INSERT INTO values_log VALUES (1)", &[])
                .unwrap()
                .command_tag
                .as_deref(),
            Some("INSERT 0 1")
        );
    }
    assert_eq!(
        engine
            .sql("SELECT * FROM values_log", &[])
            .unwrap()
            .command_tag
            .as_deref(),
        Some("SELECT 3")
    );
}

#[test]
fn simple_query_final_completion_follows_durable_commit() {
    let directory = tempfile::tempdir().unwrap();
    let engine = Engine::open(&directory.path().join("completion.uqa")).unwrap();
    let observer = engine.new_session().unwrap();
    let mut seen = Vec::new();
    engine
        .sql_simple_query(
            "CREATE TABLE committed_values (id integer); INSERT INTO committed_values VALUES (1)",
            &[],
            |result| {
                seen.push((result.command_tag.clone(), engine.transaction_depth()));
                if result.command_tag.as_deref() == Some("INSERT 0 1") {
                    assert_eq!(
                        observer
                            .sql("SELECT id FROM committed_values", &[])?
                            .rows
                            .len(),
                        1
                    );
                }
                Ok(())
            },
        )
        .unwrap();
    assert_eq!(
        seen,
        vec![
            (Some("CREATE TABLE".into()), 1),
            (Some("INSERT 0 1".into()), 0)
        ]
    );
}

#[test]
fn simple_query_withholds_final_completion_after_deferred_constraint_failure() {
    let engine = Engine::new();
    engine.sql("CREATE TABLE parent (id integer PRIMARY KEY); CREATE TABLE child (id integer REFERENCES parent DEFERRABLE INITIALLY DEFERRED)", &[]).unwrap();
    let mut seen = Vec::new();
    let error = engine
        .sql_simple_query("INSERT INTO child VALUES (9); SELECT 1", &[], |result| {
            seen.push(result.command_tag.clone());
            Ok(())
        })
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("23503"));
    assert_eq!(seen, vec![Some("INSERT 0 1".into())]);
    assert_eq!(engine.transaction_depth(), 0);
    assert!(engine
        .sql("SELECT * FROM child", &[])
        .unwrap()
        .rows
        .is_empty());
}

#[test]
fn simple_query_consumer_failure_rolls_back_implicit_segment() {
    let engine = Engine::new();
    engine
        .sql("CREATE TABLE consumer_values (id integer)", &[])
        .unwrap();
    let error = engine
        .sql_simple_query(
            "INSERT INTO consumer_values VALUES (1); INSERT INTO consumer_values VALUES (2)",
            &[],
            |_| {
                Err(SQLError::Routine {
                    sqlstate: "57014".into(),
                    message: "consumer stopped".into(),
                })
            },
        )
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("57014"));
    assert_eq!(engine.transaction_depth(), 0);
    assert!(engine
        .sql("SELECT * FROM consumer_values", &[])
        .unwrap()
        .rows
        .is_empty());
}

#[test]
fn transaction_failure_state_tracks_savepoint_recovery_and_failed_commit() {
    let engine = Engine::new();
    engine.sql("BEGIN; SAVEPOINT recovery", &[]).unwrap();
    assert!(!engine.transaction_failed());
    engine.sql("SELECT 1 / 0", &[]).unwrap_err();
    assert!(engine.transaction_failed());
    engine.sql("ROLLBACK TO recovery", &[]).unwrap();
    assert!(!engine.transaction_failed());
    engine.sql("SELECT 1 / 0", &[]).unwrap_err();
    let result = engine.sql("COMMIT", &[]).unwrap();
    assert_eq!(result.command_tag.as_deref(), Some("ROLLBACK"));
    assert_eq!(engine.transaction_depth(), 0);
    assert!(!engine.transaction_failed());
}
