//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::tests::providers::Provider;
use uqa_core::Value;

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn named_explanations_retain_their_prepared_inputs_and_native_effects(#[case] provider: Provider) {
    let reference: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../tests/parity/pg18/explain_named_inputs.expected.json"
    ))
    .unwrap();
    let directory = tempfile::tempdir().unwrap();
    for case in reference["cases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let engine = provider.open(&directory.path().join(format!("{name}.db")));
        engine
            .sql(&format!("BEGIN; {}", case["setup"].as_str().unwrap()), &[])
            .unwrap();
        let result = engine
            .sql(case["sql"].as_str().unwrap(), &[])
            .unwrap_or_else(|error| panic!("{name}: {error}"));
        let Some(Value::Json(text)) = result.value_at(0, 0) else {
            panic!("{name}: EXPLAIN JSON carrier");
        };
        let document: serde_json::Value = serde_json::from_str(text).unwrap();
        let plan = &document[0]["Plan"];
        assert_eq!(
            plan["Plan Rows"].as_f64(),
            case["rows"].as_f64(),
            "{name}: {plan}"
        );
        assert_eq!(plan["Plan Width"], case["width"], "{name}: {plan}");
        assert_eq!(
            plan.get("Actual Rows").and_then(serde_json::Value::as_f64),
            case["actual_rows"].as_f64(),
            "{name}: {plan}"
        );
        if let Some(sql) = case["after"].as_str() {
            assert_eq!(
                engine.sql(sql, &[]).unwrap().value_at(0, 0),
                Some(&Value::Int(case["next_value"].as_i64().unwrap())),
                "{name}"
            );
        }
        engine.sql("ROLLBACK", &[]).unwrap();
        engine.close().unwrap();
    }
}
