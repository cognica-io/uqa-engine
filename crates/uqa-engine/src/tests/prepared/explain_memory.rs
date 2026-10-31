//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::tests::providers::Provider;
use serde_json::Value as Json;
use uqa_core::Value;

fn text(result: &uqa_sql::SQLResult) -> String {
    (0..result.rows.len())
        .map(|row| match result.value_at(row, 0) {
            Some(Value::Str(text) | Value::Json(text)) => text.as_str(),
            other => panic!("EXPLAIN output: {other:?}"),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn memory(output: &str, format: &str) -> Option<(u64, u64)> {
    if format == "JSON" {
        let document: Json = serde_json::from_str(output).unwrap();
        let planning = &document[0]["Planning"];
        return match (
            planning.get("Memory Used"),
            planning.get("Memory Allocated"),
        ) {
            (None, None) => None,
            (Some(used), Some(allocated)) => {
                Some((used.as_u64().unwrap(), allocated.as_u64().unwrap()))
            }
            other => panic!("incomplete planning memory: {other:?}"),
        };
    }
    let (used, allocated) = match format {
        "TEXT" => (r"Memory: used=(\d+)kB", r"allocated=(\d+)kB"),
        "XML" => (
            r"<Memory-Used>(\d+)</Memory-Used>",
            r"<Memory-Allocated>(\d+)</Memory-Allocated>",
        ),
        "YAML" => (
            r#""?Memory Used"?: (\d+)"#,
            r#""?Memory Allocated"?: (\d+)"#,
        ),
        other => panic!("reference format {other}"),
    };
    let read = |pattern| {
        regex::Regex::new(pattern)
            .unwrap()
            .captures(output)
            .map(|capture| capture[1].parse::<u64>().unwrap())
    };
    match (read(used), read(allocated)) {
        (None, None) => None,
        (Some(used), Some(allocated)) => Some((used, allocated)),
        other => panic!("incomplete planning memory: {other:?}"),
    }
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_explain_memory_matches_postgresql_visibility_and_effects(#[case] provider: Provider) {
    let reference: Json = serde_json::from_str(include_str!(
        "../../../../../tests/parity/pg18/explain_memory.expected.json"
    ))
    .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("explain-memory.db"));
    let mut observations = 0;
    for (index, case) in reference["cases"].as_array().unwrap().iter().enumerate() {
        let sql = case["sql"].as_str().unwrap();
        let result = engine
            .sql(sql, &[])
            .unwrap_or_else(|error| panic!("{provider:?}, case {index}, {sql}: {error}"));
        if let Some(format) = case["format"].as_str() {
            let output = text(&result);
            let memory = memory(&output, format);
            assert_eq!(
                memory.is_some(),
                case["observation"]["memory"].as_bool().unwrap(),
                "{provider:?}, case {index}, {sql}: {output}"
            );
            if let Some((used, allocated)) = memory {
                assert!(
                    used > 0 && used <= allocated,
                    "{provider:?}, case {index}: {output}"
                );
            }
            observations += 1;
        } else if let Some(rows) = case["result"]["rows"].as_array() {
            assert_eq!(
                result.rows.len(),
                rows.len(),
                "{provider:?}, case {index}, {sql}"
            );
            for (row, values) in rows.iter().enumerate() {
                for (column, expected) in values.as_array().unwrap().iter().enumerate() {
                    let actual = match result.value_at(row, column).unwrap() {
                        Value::Int(value) => value.to_string(),
                        Value::Bool(value) => if *value { "t" } else { "f" }.into(),
                        other => panic!("unhandled reference cell: {other:?}"),
                    };
                    assert_eq!(
                        Some(actual.as_str()),
                        expected.as_str(),
                        "{provider:?}, case {index}, {sql}"
                    );
                }
            }
        }
    }
    assert_eq!(observations, 188);
    engine.close().unwrap();
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_explain_memory_precedes_execution_and_recovers_after_rejection(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("explain-memory-recovery.db");
    let engine = provider.open(&path);
    engine
        .sql("CREATE TABLE memory_effect (n integer)", &[])
        .unwrap();
    let retained = engine.query_retention_control().unwrap();
    let allowance = retained.memory();
    for mode in ["force_custom_plan", "force_generic_plan"] {
        engine
            .sql(&format!("SET plan_cache_mode = {mode}"), &[])
            .unwrap();
        for explicit in [false, true] {
            let statement = engine.prepare_statement("EXPLAIN (MEMORY, ANALYZE, FORMAT JSON) INSERT INTO memory_effect SELECT generate_series(1, 16) RETURNING repeat('x', n)", &[]).unwrap();
            if explicit {
                engine.sql("BEGIN; SAVEPOINT retry_memory", &[]).unwrap();
            }
            let full = allowance.reserve(allowance.available()).unwrap();
            let error = engine
                .execute_prepared_statement(&statement, &[])
                .unwrap_err();
            assert_eq!(error.sqlstate(), Some("53200"), "{provider:?}: {error}");
            drop(full);
            if explicit {
                assert!(engine.transaction_failed());
                engine.sql("ROLLBACK TO retry_memory", &[]).unwrap();
            } else {
                assert_eq!(engine.transaction_depth(), 0);
            }
            assert_eq!(
                engine
                    .sql("SELECT count(*) FROM memory_effect", &[])
                    .unwrap()
                    .value_at(0, 0),
                Some(&Value::Int(0))
            );
            let result = engine.execute_prepared_statement(&statement, &[]).unwrap();
            assert!(memory(&text(&result), "JSON").is_some());
            assert_eq!(
                engine
                    .sql("SELECT count(*) FROM memory_effect", &[])
                    .unwrap()
                    .value_at(0, 0),
                Some(&Value::Int(16))
            );
            drop(result);
            engine.finalize_prepared_statement(&statement);
            engine
                .sql(
                    if explicit {
                        "ROLLBACK"
                    } else {
                        "DELETE FROM memory_effect"
                    },
                    &[],
                )
                .unwrap();
        }
    }
    let query = "SELECT repeat('x', n) FROM generate_series(1, 2048) t(n)";
    let plain = engine
        .sql(
            &format!("EXPLAIN (MEMORY, ANALYZE FALSE, FORMAT JSON) {query}"),
            &[],
        )
        .unwrap();
    let analyzed = engine
        .sql(
            &format!("EXPLAIN (MEMORY, ANALYZE TRUE, FORMAT JSON) {query}"),
            &[],
        )
        .unwrap();
    assert_eq!(
        memory(&text(&plain), "JSON"),
        memory(&text(&analyzed), "JSON")
    );
    drop((plain, analyzed));
    engine
        .sql(
            "EXPLAIN (MEMORY) SELECT CASE WHEN false THEN 1 / 0 ELSE 1 END",
            &[],
        )
        .unwrap();
    assert_eq!(
        engine
            .sql("EXPLAIN (MEMORY) SELECT 1 / 0", &[])
            .unwrap_err()
            .sqlstate(),
        Some("22012")
    );
    engine.close().unwrap();
    drop(engine);
    assert_eq!(allowance.used(), 0);
    if !matches!(provider, Provider::Memory) {
        let reopened = provider.open(&path);
        assert_eq!(
            reopened
                .sql("SELECT count(*) FROM memory_effect", &[])
                .unwrap()
                .value_at(0, 0),
            Some(&Value::Int(0))
        );
        reopened.close().unwrap();
    }
}
