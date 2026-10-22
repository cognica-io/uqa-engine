//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! PostgreSQL's namespace permission errors precede argument typing and folding.

use super::*;

#[test]
fn lateral_outer_join_permissions_match_postgresql_diagnostics() {
    let reference: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../../tests/parity/pg18/lateral_functions_oracle.expected.json"
    ))
    .unwrap();
    let context = crate::binding::fixture::empty_binding_context();
    for case in reference["cases"].as_array().unwrap() {
        if !case["id"].as_str().unwrap().starts_with("forbidden_") {
            continue;
        }
        let sql = case["sql"].as_str().unwrap();
        let plan = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0));
        let error = infer_prepared_parameter_types(&NoRoutines, &plan, &[], &context).unwrap_err();
        assert_eq!(
            serde_json::json!({
                "sqlstate": error.sqlstate(),
                "message": error.to_string(),
                "detail": error.detail(),
                "hint": error.hint(),
            }),
            case["error"],
            "{sql}"
        );
    }
}
