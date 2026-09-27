//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::{ast::Statement, schema::indexes::options::validate_index_options, SQLError};
use serde_json::{json, Value};

fn diagnostic(result: Result<(), SQLError>) -> Value {
    match result {
        Ok(()) => json!({"state": null}),
        Err(error) => {
            let (detail, hint) = match &error {
                SQLError::Diagnostic { detail, hint, .. } => (detail.as_deref(), hint.as_deref()),
                _ => (None, None),
            };
            json!({"state": error.sqlstate(), "message": error.to_string(), "detail": detail, "hint": hint})
        }
    }
}

#[test]
fn relational_index_options_match_independent_postgresql_declarations() {
    let reference: Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/parity/pg18/index_definition_order.expected.json"
    )))
    .unwrap();
    let mut checked = 0;
    for case in reference["cases"].as_array().unwrap() {
        let sql = case["sql"].as_str().unwrap();
        if !sql.starts_with("CREATE INDEX new_index ON index_validation") {
            continue;
        }
        let Statement::CreateIndex(statement) = crate::compiler::compile(sql).unwrap().remove(0)
        else {
            panic!("index statement")
        };
        assert_eq!(
            diagnostic(validate_index_options(&statement)),
            case["expected"],
            "{}: {sql}",
            case["label"]
        );
        checked += 1;
    }
    assert_eq!(checked, 37);
}

#[test]
fn option_namespace_and_bare_boolean_survive_compilation() {
    let Statement::CreateIndex(statement) = crate::compiler::compile(
        "CREATE INDEX i ON t(v) WITH (hello.fillfactor=80, deduplicate_items)",
    )
    .unwrap()
    .remove(0) else {
        panic!("index statement")
    };
    assert_eq!(statement.option_namespaces, ["hello"]);
    assert_eq!(
        statement.options,
        [
            ("fillfactor".into(), "80".into()),
            ("deduplicate_items".into(), "true".into())
        ]
    );
    let encoded = serde_json::to_value(&statement).unwrap();
    let decoded: crate::ast::CreateIndex = serde_json::from_value(encoded).unwrap();
    assert_eq!(decoded.option_namespaces, statement.option_namespaces);
}

#[test]
fn gin_analyzer_option_preserves_case_insensitive_names() {
    for name in ["analyzer", "ANALYZER", "Analyzer"] {
        let Statement::CreateIndex(statement) = crate::compiler::compile(&format!(
            "CREATE INDEX i ON t USING gin(v) WITH (\"{name}\"='keyword')"
        ))
        .unwrap()
        .remove(0) else {
            panic!("index statement")
        };
        validate_index_options(&statement).unwrap();
        assert_eq!(statement.options, [(name.into(), "keyword".into())]);
    }
    let Statement::CreateIndex(statement) = crate::compiler::compile(
        "CREATE INDEX i ON t USING gin(v) WITH (analyzer='standard', \"ANALYZER\"='keyword')",
    )
    .unwrap()
    .remove(0) else {
        panic!("index statement")
    };
    let error = validate_index_options(&statement).unwrap_err();
    assert_eq!(error.sqlstate(), Some("22023"));
    assert_eq!(
        error.to_string(),
        "parameter \"ANALYZER\" specified more than once"
    );
}
