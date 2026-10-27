//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::tests::providers::Provider;

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn explained_descriptions_options_and_effects_match_postgresql(#[case] provider: Provider) {
    let reference = serde_json::from_str(include_str!(
        "../../../../../tests/parity/pg18/explain_options.expected.json"
    ))
    .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("explain-options.db"));
    super::reference::verify(&engine, &reference);
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn analyze_discards_query_rows_with_bounded_live_output(#[case] provider: Provider) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("explain-consumer.db"));
    // Initialize the session before measuring output retention. The explained rows contain over 64 MiB of text; a collecting destination cannot satisfy this bound.
    engine.sql("SELECT 1", &[]).unwrap();
    let allocation = allocation_counter::measure(|| {
        let result = engine
            .sql(
                "EXPLAIN (ANALYZE, FORMAT JSON, TIMING FALSE, SUMMARY FALSE) \
                 SELECT repeat('x', 8192) || n::text FROM generate_series(1, 8192) AS g(n)",
                &[],
            )
            .unwrap();
        let uqa_core::Value::Json(text) = result.value_at(0, 0).unwrap() else {
            panic!("EXPLAIN did not return JSON");
        };
        let document: serde_json::Value = serde_json::from_str(text).unwrap();
        assert_eq!(document[0]["Plan"]["Actual Rows"], 8192);
    });
    assert!(allocation.bytes_max < 16 * 1024 * 1024, "{allocation:?}");
    assert_eq!(engine.transaction_depth(), 0);

    engine
        .sql("CREATE TABLE explain_effects (n integer)", &[])
        .unwrap();
    engine.sql("BEGIN", &[]).unwrap();
    let result = engine
        .sql(
            "EXPLAIN (ANALYZE, FORMAT JSON) \
             INSERT INTO explain_effects SELECT generate_series(1, 17) RETURNING n",
            &[],
        )
        .unwrap();
    let uqa_core::Value::Json(text) = result.value_at(0, 0).unwrap() else {
        panic!("EXPLAIN did not return JSON");
    };
    let document: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(document[0]["Plan"]["Actual Rows"], 17);
    assert_eq!(document[0]["Plan"]["Affected Rows"], 17);
    engine.sql("ROLLBACK", &[]).unwrap();
    assert_eq!(
        engine
            .sql("SELECT count(*) FROM explain_effects", &[])
            .unwrap()
            .value_at(0, 0),
        Some(&uqa_core::Value::Int(0))
    );
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn explain_effective_settings_and_summary_match_postgresql(#[case] provider: Provider) {
    let reference: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../tests/parity/pg18/explain_settings.expected.json"
    ))
    .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("explain-settings.db"));
    for case in reference["cases"].as_array().unwrap().iter().skip(1) {
        let sql = case["sql"].as_str().unwrap();
        let result = engine
            .sql(sql, &[])
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
        if case.get("observation").is_none() {
            continue;
        }
        let uqa_core::Value::Json(text) = result.value_at(0, 0).unwrap() else {
            panic!("EXPLAIN did not return JSON");
        };
        let document: serde_json::Value = serde_json::from_str(text).unwrap();
        let document = &document[0];
        let observed = serde_json::json!({
            "settings": document.get("Settings"),
            "planning_time": document.get("Planning Time").is_some(),
            "execution_time": document.get("Execution Time").is_some(),
        });
        assert_eq!(observed, case["observation"], "{sql}");
        for key in ["Planning Time", "Execution Time"] {
            if let Some(value) = document.get(key) {
                assert!(value
                    .as_f64()
                    .is_some_and(|value| value.is_finite() && value >= 0.0));
            }
        }
    }
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn analyze_cancellation_recovers_query_and_returning_mutation_effects(#[case] provider: Provider) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("explain-cancel.db"));
    let cancellation = engine.cancellation_token();
    engine
        .register_scalar_function("explain_cancel", move |args: &[uqa_core::Value]| {
            if args == [uqa_core::Value::Int(7)] {
                cancellation.cancel();
            }
            Ok(args[0].clone())
        })
        .unwrap();
    engine
        .sql("CREATE TABLE explain_cancelled (n integer)", &[])
        .unwrap();
    for sql in [
        "EXPLAIN ANALYZE WITH written AS (INSERT INTO explain_cancelled SELECT generate_series(1, 17) RETURNING n) SELECT explain_cancel(n) FROM written",
        "EXPLAIN ANALYZE INSERT INTO explain_cancelled SELECT generate_series(1, 17) RETURNING explain_cancel(n)",
    ] {
        engine.sql("BEGIN; SAVEPOINT retry", &[]).unwrap();
        let error = engine.sql(sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("57014"), "{sql}: {error}");
        assert!(engine.transaction_failed());
        engine.reset_cancellation();
        engine.sql("ROLLBACK TO retry", &[]).unwrap();
        assert_eq!(engine.sql("SELECT count(*) FROM explain_cancelled", &[]).unwrap().value_at(0, 0), Some(&uqa_core::Value::Int(0)));
        engine.sql("COMMIT", &[]).unwrap();
    }
}
