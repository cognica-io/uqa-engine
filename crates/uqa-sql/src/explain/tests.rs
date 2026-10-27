//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use serde_json::Value;

#[test]
fn descriptions_and_ordered_option_errors_match_postgresql() {
    let reference: Value = serde_json::from_str(include_str!(
        "../../../../tests/parity/pg18/explain_options.expected.json"
    ))
    .unwrap();
    let cases = reference["cases"].as_array().unwrap();
    for (index, case) in cases.iter().enumerate() {
        if case["action"] != "prepare" {
            continue;
        }
        let sql = case["sql"].as_str().unwrap();
        let crate::Statement::Explain { options, .. } = crate::compile(sql).unwrap().remove(0)
        else {
            panic!("expected EXPLAIN syntax");
        };
        if case["result"]["status"] == 7 {
            let error = options.result_schema().unwrap_err();
            assert_eq!(
                error.sqlstate(),
                case["result"]["sqlstate"].as_str(),
                "{sql}"
            );
            assert_eq!(
                error.to_string(),
                case["result"]["message"].as_str().unwrap(),
                "{sql}"
            );
            continue;
        }
        let description = &cases[index + 1]["result"]["columns"];
        let schema = options.result_schema().unwrap();
        assert_eq!(schema.columns(), &[description[0][0].as_str().unwrap()]);
        assert_eq!(
            crate::catalog::type_metadata::pg_type_oid(schema.column_types()[0].as_ref().unwrap()),
            description[0][1].as_i64().unwrap()
        );
        let execution = &cases[index + 2]["result"];
        match options.interpret() {
            Ok(_) => assert_eq!(execution["sqlstate"], "", "{sql}"),
            Err(error) => {
                assert_eq!(error.sqlstate(), execution["sqlstate"].as_str(), "{sql}");
                assert_eq!(
                    error.to_string(),
                    execution["message"].as_str().unwrap(),
                    "{sql}"
                );
            }
        }
    }
}

#[test]
fn stored_explain_statements_keep_legacy_options_and_new_occurrence_order() {
    let statement =
        crate::compile("EXPLAIN (ANALYZE true, ANALYZE false, FORMAT xml, FORMAT json) SELECT 1")
            .unwrap()
            .remove(0);
    let stored = serde_json::to_value(&statement).unwrap();
    let restored: crate::Statement = serde_json::from_value(stored.clone()).unwrap();
    let crate::Statement::Explain { options, .. } = &restored else {
        panic!("EXPLAIN");
    };
    assert_eq!(options.options.len(), 4);
    assert!(!options.interpret().unwrap().analyze);
    assert_eq!(options.interpret().unwrap().format, ExplainFormat::Json);
    let mut legacy = stored;
    let fields = legacy["Explain"].as_object_mut().unwrap();
    fields.remove("options");
    fields.insert("analyze".into(), true.into());
    fields.insert("verbose".into(), true.into());
    fields.insert("format".into(), "xml".into());
    let restored: crate::Statement = serde_json::from_value(legacy).unwrap();
    let crate::Statement::Explain { options, .. } = &restored else {
        panic!("EXPLAIN");
    };
    let settings = options.interpret().unwrap();
    assert!(settings.analyze && settings.verbose);
    assert_eq!(settings.format, ExplainFormat::Xml);
    let plan = crate::plan::UnifiedPlan::lower(restored);
    let encoded = serde_json::to_value(&plan).unwrap();
    let decoded: crate::plan::UnifiedPlan = serde_json::from_value(encoded.clone()).unwrap();
    assert_eq!(serde_json::to_value(decoded).unwrap(), encoded);
}
