//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn boolean_test_analysis_matches_postgresql_errors_before_evaluation() {
    let cases: serde_json::Value =
        serde_json::from_str(include_str!("boolean_tests.json")).unwrap();
    for case in cases.as_array().unwrap() {
        let sql = case["sql"].as_str().unwrap();
        let plan = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0));
        let error = infer_prepared_parameter_types(&NoRoutines, &plan, &[], &assignment_context())
            .unwrap_err();
        assert_eq!(
            error.sqlstate(),
            case["error"]["sqlstate"].as_str(),
            "{sql}: {error}"
        );
        assert_eq!(
            error.to_string(),
            case["error"]["message"].as_str().unwrap(),
            "{sql}"
        );
    }
}

#[test]
fn boolean_tests_infer_parameters_without_retyping_declared_inputs() {
    use crate::ast::BooleanTest;
    for test in [
        BooleanTest::IsTrue,
        BooleanTest::IsNotTrue,
        BooleanTest::IsFalse,
        BooleanTest::IsNotFalse,
        BooleanTest::IsUnknown,
        BooleanTest::IsNotUnknown,
    ] {
        let sql = format!("SELECT $1 {}", test.label());
        let plan = UnifiedPlan::lower(crate::compile(&sql).unwrap().remove(0));
        let types =
            infer_prepared_parameter_types(&NoRoutines, &plan, &[None], &assignment_context())
                .unwrap();
        assert_eq!(types, vec![Some(ColumnType::Boolean)]);
        let error = infer_prepared_parameter_types(
            &NoRoutines,
            &plan,
            &[Some(ColumnType::Text)],
            &assignment_context(),
        )
        .unwrap_err();
        assert_eq!(error.sqlstate(), Some("42804"));
        assert_eq!(
            error.to_string(),
            format!(
                "argument of {} must be type boolean, not type text",
                test.label()
            )
        );
    }
}
