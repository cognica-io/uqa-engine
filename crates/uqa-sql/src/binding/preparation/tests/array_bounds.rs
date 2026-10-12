//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn array_bound_analysis_matches_postgresql_type_and_error_order() {
    let cases: serde_json::Value = serde_json::from_str(include_str!("array_bounds.json")).unwrap();
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
fn array_bounds_infer_integer_parameters_without_retyping_declared_text() {
    for sql in [
        "SELECT (ARRAY[1,2,3])[$1:$2]",
        "SELECT (ARRAY[[1,2],[3,4]])[$1][$2]",
    ] {
        let plan = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0));
        let types = infer_prepared_parameter_types(
            &NoRoutines,
            &plan,
            &[None, None],
            &assignment_context(),
        )
        .unwrap();
        assert_eq!(
            types,
            vec![Some(ColumnType::Integer), Some(ColumnType::Integer)]
        );
        let error = infer_prepared_parameter_types(
            &NoRoutines,
            &plan,
            &[Some(ColumnType::Text), None],
            &assignment_context(),
        )
        .unwrap_err();
        assert_eq!(error.sqlstate(), Some("42804"));
        assert_eq!(error.to_string(), "array subscript must have type integer");
    }
}
