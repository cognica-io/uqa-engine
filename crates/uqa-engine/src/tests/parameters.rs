//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::providers::Provider;
use serde_json::{json, Value as Json};
use uqa_core::{
    memory::{MemoryBudget, ProductionControl},
    CancellationToken, Value,
};
use uqa_sql::{SQLError, SQLResult};

mod io_timing;
mod live;

fn rows(engine: &crate::Engine, result: &SQLResult) -> Result<Json, SQLError> {
    result
        .rows
        .iter()
        .map(|row| {
            result
                .columns
                .iter()
                .zip(&result.column_types)
                .map(|(name, ty)| match &row[name] {
                    Value::Null => Ok(Json::Null),
                    value => uqa_sql::result::format_postgres_text(
                        value,
                        ty.as_ref().unwrap(),
                        Some(engine),
                    )
                    .map(Json::from),
                })
                .collect::<Result<Json, SQLError>>()
        })
        .collect()
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn host_type_input_and_transaction_effects_match_postgresql(#[case] provider: Provider) {
    let reference: Json = serde_json::from_str(include_str!("parameters.pg18.json")).unwrap();
    assert!(reference["provenance"]["version"]
        .as_str()
        .unwrap()
        .starts_with("PostgreSQL 18."));
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("parameters.db"));
    let control = ProductionControl::uncontrolled();
    for (index, case) in reference["cases"].as_array().unwrap().iter().enumerate() {
        let result = if let Some(sql) = case["sql"].as_str() {
            engine
                .sql(sql, &[])
                .and_then(|result| rows(&engine, &result))
        } else {
            engine
                .create_typed_parameter_with_control(
                    case["type"].as_str().unwrap(),
                    case["input"].as_str(),
                    &control,
                )
                .and_then(|parameter| {
                    // Binding an admitted domain must not execute its input CHECK again.
                    engine
                        .sql("SELECT $1 AS value", std::slice::from_ref(&*parameter))
                        .and_then(|result| rows(&engine, &result))
                })
        };
        match result {
            Ok(actual) => {
                assert_eq!(case["sqlstate"], "", "case {index}: {case}");
                if let Some(expected) = case.get("rows") {
                    assert_eq!(&actual, expected, "case {index}: {case}");
                }
            }
            Err(error) => {
                assert_ne!(
                    case["sqlstate"], "",
                    "unexpected error at case {index}: {error}"
                );
                assert_eq!(
                    json!(error.sqlstate().unwrap_or_default()),
                    case["sqlstate"],
                    "case {index}: {case}: {error}"
                );
                assert_eq!(
                    json!(error.to_string()),
                    case["message"],
                    "case {index}: {case}"
                );
            }
        }
        let state = if engine.transaction_failed() {
            3
        } else if engine.transaction_depth() > 0 {
            2
        } else {
            0
        };
        assert_eq!(
            json!(state),
            case["transaction_status"],
            "case {index}: {case}"
        );
    }
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn controlled_input_failure_uses_the_owning_transaction_frame(#[case] provider: Provider) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("control.db"));
    // Include admitted type-name resolution scratch while keeping the 4 KiB
    // text payload outside the allowance.
    let budget = MemoryBudget::new(1024);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    let held = control.copy_text("held").unwrap();
    engine.sql("BEGIN; SAVEPOINT input_retry", &[]).unwrap();
    assert_eq!(
        engine
            .create_typed_parameter_with_control("text", Some(&"x".repeat(4096)), &control,)
            .unwrap_err()
            .sqlstate(),
        Some("53200")
    );
    assert!(engine.transaction_failed());
    assert_eq!(budget.used(), held.reserved_bytes());
    assert_eq!(
        engine
            .create_typed_parameter_with_control("integer", Some("1"), &control,)
            .unwrap_err()
            .sqlstate(),
        Some("25P02")
    );
    engine.rollback_to_savepoint("input_retry").unwrap();
    for token in [&original, &invoking] {
        token.cancel();
        assert_eq!(
            engine
                .create_typed_parameter_with_control("integer", Some("1"), &control,)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        assert!(engine.transaction_failed());
        assert_eq!(budget.used(), held.reserved_bytes());
        token.reset();
        engine.rollback_to_savepoint("input_retry").unwrap();
    }
    let value = engine
        .create_typed_parameter_with_control("integer", Some("1"), &control)
        .unwrap();
    assert_eq!(value.scalar_value(), Some(&Value::Int(1)));
    engine.rollback().unwrap();
    assert_eq!(engine.transaction_depth(), 0);
}
