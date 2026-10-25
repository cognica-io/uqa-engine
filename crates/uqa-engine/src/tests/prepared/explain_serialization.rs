//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::{tests::providers::Provider, Engine};
use uqa_core::Value;

fn serialization(engine: &Engine, sql: &str) -> Option<serde_json::Value> {
    let result = engine
        .sql(sql, &[])
        .unwrap_or_else(|error| panic!("{sql}: {error}"));
    let Value::Json(text) = result.value_at(0, 0).unwrap() else {
        panic!("EXPLAIN JSON carrier");
    };
    let payload: serde_json::Value = serde_json::from_str(text).unwrap();
    payload[0].get("Serialization").cloned()
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn serialization_volumes_and_option_visibility_match_postgresql(#[case] provider: Provider) {
    let reference: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../tests/parity/pg18/explain_serialization.expected.json"
    ))
    .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("explain-serialization.db"));
    for case in reference["cases"].as_array().unwrap() {
        engine.sql("BEGIN", &[]).unwrap();
        let setup = case["setup"].as_str().unwrap();
        if !setup.is_empty() {
            engine.sql(setup, &[]).unwrap();
        }
        let sql = case["sql"].as_str().unwrap();
        if let Some(state) = case["sqlstate"].as_str() {
            let error = engine.sql(sql, &[]).unwrap_err();
            assert_eq!(error.sqlstate(), Some(state), "{sql}: {error}");
            let uqa_sql::SQLError::Routine { message, .. } = error.primary() else {
                panic!("{error}");
            };
            assert_eq!(message, case["message"].as_str().unwrap(), "{sql}");
            engine.sql("ROLLBACK", &[]).unwrap();
            continue;
        }
        let observed = serialization(&engine, sql);
        let expected = &case["serialization"];
        if expected.is_null() {
            assert!(observed.is_none(), "{sql}");
        } else {
            let observed = observed.unwrap();
            assert_eq!(observed["Format"], expected["Format"], "{sql}");
            assert_eq!(
                observed["Output Volume"], expected["Output Volume"],
                "{sql}"
            );
            assert_eq!(
                observed.get("Time").is_some(),
                expected["timing"].as_bool().unwrap_or(false),
                "{sql}"
            );
            if let Some(elapsed) = observed.get("Time") {
                assert!(elapsed.as_f64().unwrap() >= 0.0);
            }
        }
        engine.sql("ROLLBACK", &[]).unwrap();
    }
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn serialization_is_incremental_and_conversion_failure_aborts_the_statement(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("explain-serialization-failure.db"));
    engine
        .sql("CREATE TABLE serialized_effects (n integer)", &[])
        .unwrap();
    engine.sql("SELECT 1", &[]).unwrap();
    let allocation = allocation_counter::measure(|| {
        let observed = serialization(&engine, "EXPLAIN (ANALYZE, SERIALIZE TEXT, FORMAT JSON, TIMING OFF) SELECT repeat('x',8192) FROM generate_series(1,8192)").unwrap();
        assert_eq!(
            observed["Output Volume"],
            (8192_u64 * (8192 + 6)).div_ceil(1024)
        );
    });
    assert!(allocation.bytes_max < 16 * 1024 * 1024, "{allocation:?}");
    let error = engine.sql("EXPLAIN (ANALYZE, SERIALIZE BINARY) INSERT INTO serialized_effects VALUES (1) RETURNING NULL::aclitem", &[]).unwrap_err();
    assert_eq!(error.sqlstate(), Some("42883"));
    assert!(engine
        .sql("SELECT * FROM serialized_effects", &[])
        .unwrap()
        .rows
        .is_empty());
    engine
        .sql("BEGIN; SAVEPOINT before_serialize", &[])
        .unwrap();
    assert!(engine.sql("EXPLAIN (ANALYZE, SERIALIZE BINARY) INSERT INTO serialized_effects VALUES (2) RETURNING NULL::aclitem", &[]).is_err());
    engine
        .sql(
            "ROLLBACK TO before_serialize; INSERT INTO serialized_effects VALUES (3); COMMIT",
            &[],
        )
        .unwrap();
    assert_eq!(
        engine
            .sql("SELECT n FROM serialized_effects", &[])
            .unwrap()
            .value_at(0, 0),
        Some(&Value::Int(3))
    );
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn serialization_renderers_share_measured_fields_and_timing_visibility(#[case] provider: Provider) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("explain-serialization-formats.db"));
    for format in ["TEXT", "XML", "YAML"] {
        for timing in [false, true] {
            let sql = format!("EXPLAIN (ANALYZE, SERIALIZE BINARY, FORMAT {format}, TIMING {timing}, SUMMARY FALSE) SELECT n::bigint FROM generate_series(1,257) AS g(n)");
            let result = engine.sql(&sql, &[]).unwrap();
            let text = result
                .rows
                .iter()
                .map(|row| match &row["QUERY PLAN"] {
                    Value::Str(text) => text.as_str(),
                    value => panic!("{value:?}"),
                })
                .collect::<Vec<_>>()
                .join("\n");
            match format {
                "TEXT" => {
                    assert!(text.contains("output=4kB  format=binary"), "{text}");
                    assert_eq!(text.contains("Serialization: time="), timing, "{text}");
                }
                "XML" => {
                    uqa_sql::xml::validate(&text, uqa_sql::xml::XmlOption::Document).unwrap();
                    assert!(text.contains("<Output-Volume>4</Output-Volume>"), "{text}");
                    assert_eq!(text.contains("<Time>"), timing, "{text}");
                }
                "YAML" => {
                    assert!(text.contains("\"Output Volume\": 4"), "{text}");
                    assert_eq!(text.contains("\"Time\":"), timing, "{text}");
                }
                _ => unreachable!(),
            }
        }
    }
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn returning_conversion_failure_stops_rows_before_after_statement_triggers(
    #[case] provider: Provider,
) {
    let reference: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../tests/parity/pg18/explain_serialization.expected.json"
    ))
    .unwrap();
    for (index, case) in reference["mutation_failures"]
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
    {
        let directory = tempfile::tempdir().unwrap();
        let engine = provider.open(&directory.path().join(format!("returning-{index}.db")));
        engine.sql(case["setup"].as_str().unwrap(), &[]).unwrap();
        engine.sql("BEGIN; SAVEPOINT before_output", &[]).unwrap();
        let error = engine.sql(case["sql"].as_str().unwrap(), &[]).unwrap_err();
        assert_eq!(
            error.sqlstate(),
            case["sqlstate"].as_str(),
            "{case}: {error}"
        );
        engine.sql("ROLLBACK TO before_output", &[]).unwrap();
        let effects = engine
            .sql(
                "SELECT currval('serialization_rows'), nextval('serialization_after')",
                &[],
            )
            .unwrap();
        assert_eq!(
            (effects.value_at(0, 0), effects.value_at(0, 1)),
            (
                Some(&Value::Int(case["effects"]["rows"].as_i64().unwrap())),
                Some(&Value::Int(case["effects"]["after"].as_i64().unwrap())),
            ),
            "{case}"
        );
        engine.sql("ROLLBACK", &[]).unwrap();
    }
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn rule_query_inputs_match_postgresql(#[case] provider: Provider) {
    let reference: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../tests/parity/pg18/rules_query_inputs.expected.json"
    ))
    .unwrap();
    let mut failures = Vec::new();
    for case in reference["cases"].as_array().unwrap() {
        let directory = tempfile::tempdir().unwrap();
        let engine = provider.open(&directory.path().join("rules.db"));
        engine.sql(case["setup"].as_str().unwrap(), &[]).unwrap();
        let recover = case["recover"].as_bool().unwrap_or(false);
        if recover {
            engine.sql("BEGIN; SAVEPOINT before_rule", &[]).unwrap();
        }
        let result = engine.sql(case["sql"].as_str().unwrap(), &[]);
        if let Some(state) = case["sqlstate"].as_str() {
            if result.as_ref().err().and_then(|error| error.sqlstate()) != Some(state) {
                failures.push(format!("{} expected {state}, got {result:?}", case["name"]));
            }
            if recover {
                engine.sql("ROLLBACK TO before_rule", &[]).unwrap();
            } else {
                continue;
            }
        }
        if !recover {
            if let Err(error) = result {
                failures.push(format!("{}: {error}", case["name"]));
                continue;
            }
        }
        let result = engine.sql(case["query"].as_str().unwrap(), &[]).unwrap();
        let Value::Json(text) = result.value_at(0, 0).unwrap() else {
            panic!("JSON result")
        };
        let observed: serde_json::Value = serde_json::from_str(text).unwrap();
        if observed != case["result"] {
            failures.push(format!(
                "{}: {observed} != {}",
                case["name"], case["result"]
            ));
        }
        if recover {
            engine.sql("ROLLBACK", &[]).unwrap();
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn serialization_receives_only_the_outer_statement_output(#[case] provider: Provider) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("serialization-nested.db"));
    engine
        .sql("CREATE TABLE nested_serialization(n bigint)", &[])
        .unwrap();
    let metrics = serialization(&engine, "EXPLAIN (ANALYZE, SERIALIZE BINARY, FORMAT JSON, TIMING OFF) WITH inserted AS (INSERT INTO nested_serialization VALUES (1),(2) RETURNING NULL::aclitem) SELECT count(*) FROM inserted").unwrap();
    assert_eq!(metrics["Output Volume"], 1);
    assert_eq!(
        engine
            .sql("SELECT count(*) FROM nested_serialization", &[])
            .unwrap()
            .value_at(0, 0),
        Some(&Value::Int(2))
    );
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn ordinary_delete_interleaves_qualification_before_trigger_and_returning(
    #[case] provider: Provider,
) {
    let reference: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../tests/parity/pg18/explain_serialization.expected.json"
    ))
    .unwrap();
    let case = &reference["mutation_order"];
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("delete-delivery-order.db"));
    engine.sql(case["setup"].as_str().unwrap(), &[]).unwrap();
    let result = engine.sql(case["sql"].as_str().unwrap(), &[]).unwrap();
    let expected = case["rows"].as_array().unwrap();
    assert_eq!(result.rows.len(), expected.len());
    for (index, row) in expected.iter().enumerate() {
        for (column, key) in ["n", "qualified", "before_row", "output_row"]
            .iter()
            .enumerate()
        {
            assert_eq!(
                result.value_at(index, column),
                Some(&Value::Int(row[key].as_i64().unwrap())),
                "row {index} column {key}"
            );
        }
    }
}
