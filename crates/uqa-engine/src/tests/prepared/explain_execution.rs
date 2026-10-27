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
fn root_execution_counts_and_timing_fields_match_postgresql(#[case] provider: Provider) {
    let reference: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../tests/parity/pg18/explain_execution.expected.json"
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
            panic!("{name}: JSON result");
        };
        let document: serde_json::Value = serde_json::from_str(text).unwrap();
        let document = &document[0];
        let plan = &document["Plan"];
        assert_eq!(
            plan["Actual Rows"].as_f64(),
            case["rows"].as_f64(),
            "{name}: {plan}"
        );
        assert_eq!(
            plan["Actual Loops"].as_f64(),
            case["loops"].as_f64(),
            "{name}: {plan}"
        );
        assert_eq!(
            plan.get("Actual Startup Time").is_some(),
            case["startup"].as_bool().unwrap(),
            "{name}"
        );
        assert_eq!(
            plan.get("Actual Total Time").is_some(),
            case["total"].as_bool().unwrap(),
            "{name}"
        );
        assert_eq!(
            document.get("Execution Time").is_some(),
            case["summary"].as_bool().unwrap(),
            "{name}"
        );
        if case["startup"] == true {
            let startup = plan["Actual Startup Time"].as_f64().unwrap();
            let total = plan["Actual Total Time"].as_f64().unwrap();
            assert!(
                startup.is_finite() && total.is_finite() && 0.0 <= startup && startup <= total,
                "{name}: {plan}"
            );
            if case["loops"] == 0 {
                assert_eq!(total, 0.0, "{name}");
            }
        }
        let executions = document["Node Executions"]
            .as_array()
            .expect("native execution records");
        for (index, execution) in executions.iter().enumerate() {
            assert_eq!(execution["Execution ID"].as_u64(), Some(index as u64));
            assert!(!execution["Node Type"].as_str().unwrap().is_empty());
            assert_eq!(
                execution.get("Actual Startup Time").is_some(),
                case["startup"].as_bool().unwrap()
            );
            assert_eq!(
                execution.get("Actual Total Time").is_some(),
                case["total"].as_bool().unwrap()
            );
            if execution["Actual Loops"] == 0 {
                assert_eq!(execution["Actual Rows"], 0);
            }
            if case["total"] == true {
                let startup = execution["Actual Startup Time"].as_f64().unwrap();
                let total = execution["Actual Total Time"].as_f64().unwrap();
                assert!(
                    startup.is_finite() && total.is_finite() && 0.0 <= startup && startup <= total,
                    "{name}: {execution}"
                );
            }
        }
        engine.sql("ROLLBACK", &[]).unwrap();
        engine.close().unwrap();
    }
}
