//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Message, cache and durable routine compilation use their `PostgreSQL` lexical boundary.

use uqa_core::Value;
use uqa_engine::Engine;

#[path = "parser_settings/sql_routine_lifetimes.rs"]
mod sql_routine_lifetimes;

fn open(provider: usize, path: &std::path::Path) -> Engine {
    match provider {
        0 => Engine::new(),
        1 => Engine::open(path).unwrap(),
        2 => Engine::from_persistent_provider(std::sync::Arc::new(
            uqa_storage_sqlite::SQLiteKeyValueStorage::open(path).unwrap(),
        ))
        .unwrap(),
        3 => Engine::from_persistent_provider(std::sync::Arc::new(
            uqa_storage_redb::RedbStorage::open(path).unwrap(),
        ))
        .unwrap(),
        _ => unreachable!(),
    }
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn parser_settings_match_postgresql_and_survive_reopen(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("parser-settings.db");
    let engine = open(provider, &path);
    let reference: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../tests/parity/pg18/parser_settings_oracle.expected.json"
    ))
    .unwrap();
    crate::pg18_oracle::verify(&engine, &reference.to_string());
    crate::pg18_oracle::verify(
        &engine,
        include_str!("../../../../tests/parity/pg18/dynamic_parser_settings_oracle.expected.json"),
    );
    if provider == 0 {
        return;
    }
    drop(engine);
    let engine = open(provider, &path);
    engine.sql("SET escape_string_warning=off", &[]).unwrap();
    let mut reopened = reference;
    reopened["cases"]
        .as_array_mut()
        .unwrap()
        .retain(|case| case["id"] == "reopen_values");
    assert_eq!(reopened["cases"].as_array().unwrap().len(), 1);
    crate::pg18_oracle::verify(&engine, &reopened.to_string());
}

#[test]
fn cached_cursor_notices_follow_current_filter_and_string_settings() {
    let engine = Engine::new();
    let sql = r"SELECT 'a\nb' AS value";
    for (setting, value, warning_count) in [
        ("SET standard_conforming_strings=off", "a\nb", 1),
        ("SET client_min_messages=error", "a\nb", 0),
        ("SET client_min_messages=warning", "a\nb", 1),
        ("SET standard_conforming_strings=on", r"a\nb", 0),
    ] {
        engine.sql(setting, &[]).unwrap();
        for _ in 0..2 {
            let cursor = engine.sql_cursor(sql, &[]).unwrap();
            let rows = cursor
                .flat_map(|batch| batch.unwrap().into_rows())
                .collect::<Vec<_>>();
            assert_eq!(rows[0]["value"], Value::Str(value.into()));
            assert_eq!(engine.take_sql_notices().len(), warning_count);
        }
    }
}

#[test]
fn independent_sessions_and_api_batches_use_live_message_settings() {
    let first = Engine::new();
    let second = Engine::new();
    let sql = r"SELECT 'a\nb' AS value";
    let results = first
        .sql_batch(&[("SET standard_conforming_strings=off", &[]), (sql, &[])])
        .unwrap();
    assert_eq!(results[1].rows[0]["value"], Value::Str("a\nb".into()));
    assert_eq!(
        second.sql(sql, &[]).unwrap().rows[0]["value"],
        Value::Str(r"a\nb".into())
    );
    assert_eq!(second.take_sql_notices().len(), 0);
}

#[test]
fn subscription_policy_admission_keeps_legacy_strings_and_single_warnings() {
    let engine = Engine::new();
    engine.require_notification_subscriptions().unwrap();
    engine
        .sql("SET standard_conforming_strings=off", &[])
        .unwrap();
    let results = engine
        .sql_batch(&[(r"SELECT 'a\'; LISTEN embedded' AS value", &[])])
        .unwrap();
    assert_eq!(
        results[0].rows[0]["value"],
        Value::Str("a'; LISTEN embedded".into())
    );
    assert_eq!(engine.take_sql_notices().len(), 1);
    engine
        .sql("CREATE SEQUENCE admission_counter", &[])
        .unwrap();
    let error = engine
        .sql_batch(&[
            ("SELECT nextval('admission_counter')", &[]),
            (r"SELECT 'a\'b'; LISTEN actual", &[]),
        ])
        .unwrap_err();
    assert_eq!(error.code(), Some("NOTIFICATION_REQUIRES_SUBSCRIPTION"));
    assert_eq!(
        engine
            .sql("SELECT nextval('admission_counter') AS value", &[])
            .unwrap()
            .rows[0]["value"],
        Value::Int(1)
    );
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn procedural_sql_is_prepared_when_each_source_occurrence_is_first_reached(
    #[case] provider: usize,
) {
    let directory = tempfile::tempdir().unwrap();
    let engine = open(provider, &directory.path().join("first-use.db"));
    crate::pg18_oracle::verify(
        &engine,
        include_str!("../../../../tests/parity/pg18/plpgsql_first_use_oracle.expected.json"),
    );
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn procedural_input_reanalysis_preserves_first_use_syntax(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let engine = open(provider, &directory.path().join("procedural-inputs.db"));
    crate::pg18_oracle::verify(
        &engine,
        include_str!("../../../../tests/parity/pg18/plpgsql_input_lifetime_oracle.expected.json"),
    );
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn procedural_cursor_scope_and_anonymous_invalidation_match_postgresql(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let engine = open(provider, &directory.path().join("procedural-scope.db"));
    crate::pg18_oracle::verify(
        &engine,
        include_str!("../../../../tests/parity/pg18/plpgsql_scope_oracle.expected.json"),
    );
}
