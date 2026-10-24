//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::Engine;
use serde_json::{json, Value as Json};

#[rstest::rstest]
#[case::memory(false)]
#[case::native_sqlite(true)]
fn explained_input_admission_matches_postgresql(#[case] persistent: bool) {
    let reference: Json = serde_json::from_str(include_str!(
        "../../../../../tests/parity/pg18/prepared_explain_inputs.expected.json"
    ))
    .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let engine = if persistent {
        Engine::open(&directory.path().join("explain.db")).unwrap()
    } else {
        Engine::new()
    };
    engine
        .sql(reference["declaration"].as_str().unwrap(), &[])
        .unwrap();
    for case in reference["cases"].as_array().unwrap() {
        engine.sql("BEGIN", &[]).unwrap();
        let sql = case["sql"].as_str().unwrap();
        let mut observed = match engine.prepare_statement(sql, &[]) {
            Ok(handle) => {
                let description = engine.describe_prepared_statement(&handle).unwrap();
                let parameters = description
                    .parameter_types
                    .iter()
                    .map(|ty| {
                        uqa_sql::catalog::result_type::postgres_result_type(ty.as_ref().unwrap())
                            .type_oid
                    })
                    .collect::<Vec<_>>();
                engine.finalize_prepared_statement(&handle);
                json!({"status": 1, "sqlstate": "", "message": "", "parameters": parameters})
            }
            Err(error) => {
                json!({"status": 7, "sqlstate": error.sqlstate().unwrap(), "message": error.to_string(), "parameters": []})
            }
        };
        observed["transaction_status"] = json!(if engine.transaction_failed() {
            3
        } else if engine.transaction_depth() != 0 {
            2
        } else {
            0
        });
        assert_eq!(observed, case["result"], "{sql}");
        engine.sql("ROLLBACK", &[]).unwrap();
        assert_eq!(engine.transaction_depth(), 0);
        assert!(engine.session.prepared.read().is_empty());
    }
    let result = engine
        .sql("SELECT count(*) AS rows FROM explain_inputs", &[])
        .unwrap();
    assert_eq!(result.rows[0]["rows"], uqa_core::Value::Int(0));
}
