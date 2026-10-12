//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn json_operand_analysis_matches_postgresql() {
    let cases: serde_json::Value =
        serde_json::from_str(include_str!("json_operators.json")).unwrap();
    let mut failures = Vec::new();
    for case in cases.as_array().unwrap() {
        let sql = case["sql"].as_str().unwrap();
        let plan = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0));
        let actual = infer_prepared_parameter_types(&NoRoutines, &plan, &[], &assignment_context());
        if case["error"].is_null() {
            if let Err(error) = actual {
                failures.push(format!("{sql}: {error}"));
            }
        } else {
            match actual {
                Err(error)
                    if error.sqlstate() == case["error"]["sqlstate"].as_str()
                        && error.to_string() == case["error"]["message"].as_str().unwrap()
                        && error.detail() == case["error"]["detail"].as_str()
                        && error.hint() == case["error"]["hint"].as_str() => {}
                actual => failures.push(format!(
                    "{sql}: actual {actual:?}; expected {}",
                    case["error"]
                )),
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
