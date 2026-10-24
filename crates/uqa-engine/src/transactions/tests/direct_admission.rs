//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Engine, SQLError};
use serde_json::{json, Value as Json};

fn reference() -> Json {
    serde_json::from_str(include_str!(
        "../../../../../tests/parity/pg18/operation_admission_oracle.expected.json"
    ))
    .unwrap()
}

fn transaction_status(engine: &Engine) -> u32 {
    if engine.transaction_failed() {
        3
    } else if engine.transaction_depth() != 0 {
        2
    } else {
        0
    }
}

fn observe_sql(engine: &Engine, sql: &str) -> Json {
    let mut result = match engine.sql(sql, &[]) {
        Ok(result) => json!({
            "sqlstate": "", "message": "",
            "value": result.value_at(0, 0).map(|value| {
                let crate::Value::Int(value) = value else { panic!("expected integer") };
                value.to_string()
            })
        }),
        Err(error) => json!({
            "sqlstate": error.sqlstate().unwrap(),
            "message": error.to_string(), "value": null
        }),
    };
    result["transaction_status"] = json!(transaction_status(engine));
    result
}

fn sequence_call(engine: &Engine, sql: &str, direct: bool, expected: &Json) {
    if !direct {
        assert_eq!(observe_sql(engine, sql), *expected, "{sql}");
        return;
    }
    let result = match sql {
        "SELECT lastval()" => engine.lastval(),
        "SELECT currval('admission_ids')" => engine.currval("admission_ids"),
        "SELECT setval('admission_ids', 3)" => engine.setval("admission_ids", 3),
        "SELECT nextval('admission_ids')" => engine.nextval("admission_ids"),
        "SELECT setval('admission_ids', 1, false)" => {
            engine.setval_with_is_called("admission_ids", 1, false)
        }
        _ => panic!("unexpected reference operation: {sql}"),
    };
    // The public direct API returns String errors, so compare its actual diagnostics and state without inventing a SQLSTATE for that API.
    let (message, value) = match result {
        Ok(value) => (String::new(), Some(value.to_string())),
        Err(message) => (message, None),
    };
    assert_eq!(json!(message), expected["message"], "{sql}");
    assert_eq!(json!(value), expected["value"], "{sql}");
    assert_eq!(
        json!(transaction_status(engine)),
        expected["transaction_status"],
        "{sql}"
    );
}

#[rstest::rstest]
#[case::memory_direct(false, true)]
#[case::sqlite_direct(true, true)]
#[case::memory_sql(false, false)]
#[case::sqlite_sql(true, false)]
fn sequence_operation_failure_and_recovery_match_pg18(
    #[case] persistent: bool,
    #[case] direct: bool,
) {
    let directory = tempfile::tempdir().unwrap();
    let engine = if persistent {
        Engine::open(&directory.path().join("sequence-admission.db")).unwrap()
    } else {
        Engine::new()
    };
    engine
        .sql("CREATE TEMP SEQUENCE admission_ids MAXVALUE 2", &[])
        .unwrap();
    let reference = reference();
    for case in reference["cases"].as_array().unwrap() {
        engine.sql("BEGIN; SAVEPOINT keep", &[]).unwrap();
        let sql = case["sql"].as_str().unwrap();
        if sql == "SELECT nextval('admission_ids')" {
            for value in [1, 2] {
                sequence_call(
                    &engine,
                    sql,
                    direct,
                    &json!({"sqlstate": "", "message": "", "value": value.to_string(), "transaction_status": 2}),
                );
            }
        }
        sequence_call(&engine, sql, direct, &case["failure"]);
        sequence_call(
            &engine,
            "SELECT setval('admission_ids', 1, false)",
            direct,
            &case["blocked"],
        );
        for (sql, field) in [
            ("ROLLBACK TO keep", "recovered"),
            ("SELECT 1", "after_recovery"),
            ("ROLLBACK", "rolled_back"),
        ] {
            assert_eq!(observe_sql(&engine, sql), case[field], "{sql}");
        }
    }
    sequence_call(
        &engine,
        "SELECT currval('admission_ids')",
        direct,
        &reference["current_value"],
    );
    sequence_call(
        &engine,
        "SELECT nextval('admission_ids')",
        direct,
        &reference["exhausted"],
    );
}

#[rstest::rstest]
#[case::memory(false)]
#[case::native_sqlite(true)]
fn read_only_rejection_is_owned_and_failed_admission_precedes_work(#[case] persistent: bool) {
    let directory = tempfile::tempdir().unwrap();
    let engine = if persistent {
        Engine::open(&directory.path().join("read-only-admission.db")).unwrap()
    } else {
        Engine::new()
    };
    engine
        .sql(
            "CREATE TABLE items (n integer); INSERT INTO items VALUES (1)",
            &[],
        )
        .unwrap();
    let reference = reference();
    let expected = &reference["read_only"];
    for route in [
        "row",
        "prepared row",
        "mutation",
        "definition",
        "mapped",
        "storage",
        "graph",
    ] {
        engine
            .sql("BEGIN; SAVEPOINT keep; SET TRANSACTION READ ONLY", &[])
            .unwrap();
        let operation = || match route {
            "row" => engine.delete_document("items", 1),
            "prepared row" => engine
                .with_prepared_row_write_transaction("items", |_| panic!("read-only callback")),
            "mutation" => engine.save_scoring_params("forbidden", "{}"),
            "definition" => {
                engine.with_implicit_definition_transaction(|_| panic!("read-only callback"))
            }
            "mapped" => engine.with_implicit_mapped_transaction(
                |_| panic!("read-only callback"),
                std::convert::identity,
            ),
            "storage" => engine
                .with_implicit_storage_transaction(|_| panic!("read-only callback"))
                .map_err(|error| Engine::storage_tx_error("test", &error)),
            "graph" => engine
                .create_graph("forbidden")
                .map(|_| ())
                .map_err(|error| Engine::storage_tx_error("test", &error)),
            _ => unreachable!(),
        };
        let error: SQLError = operation().unwrap_err();
        assert_eq!(
            json!(error.sqlstate().unwrap()),
            expected["failure"]["sqlstate"],
            "{route}: {error}"
        );
        assert_eq!(
            json!(transaction_status(&engine)),
            expected["failure"]["transaction_status"],
            "{route}"
        );
        let error = operation().unwrap_err();
        assert_eq!(
            json!(error.sqlstate().unwrap()),
            expected["blocked"]["sqlstate"],
            "{route}: {error}"
        );
        assert_eq!(observe_sql(&engine, "SELECT 1"), expected["blocked"]);
        for (sql, field) in [
            ("ROLLBACK TO keep", "recovered"),
            ("SELECT 1", "after_recovery"),
            ("ROLLBACK", "rolled_back"),
        ] {
            assert_eq!(observe_sql(&engine, sql), expected[field], "{route}: {sql}");
        }
    }
    assert_eq!(
        engine
            .sql("SELECT n FROM items", &[])
            .unwrap()
            .value_at(0, 0),
        Some(&crate::Value::Int(1))
    );
    assert!(engine.load_scoring_params("forbidden").unwrap().is_none());
}
