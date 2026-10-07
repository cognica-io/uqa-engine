//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn membership_checks_sets_only_in_boolean_comparisons_after_input_typing() {
    for sql in [
        "SELECT 1 IN (generate_series(1,2))",
        "SELECT generate_series(1,2) NOT IN (1)",
        "SELECT 1 IN (1,2,generate_series(1,id)) FROM assignment_target",
    ] {
        let plan = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0));
        let error = infer_prepared_parameter_types(&NoRoutines, &plan, &[], &assignment_context())
            .unwrap_err();
        assert_eq!(error.sqlstate(), Some("42804"), "{sql}: {error}");
        assert_eq!(error.to_string(), "argument of IN must not return a set");
    }
    for sql in [
        "SELECT 1 IN (generate_series(1,2),3)",
        "SELECT generate_series(1,2) IN (1,2)",
    ] {
        let plan = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0));
        infer_prepared_parameter_types(&NoRoutines, &plan, &[], &assignment_context()).unwrap();
    }
}

struct InapplicableUserOverload;

impl FunctionTypeResolver for InapplicableUserOverload {
    fn resolve_function_type(
        &self,
        _: &str,
        _: Option<&FunctionBinding>,
        _: &[Option<String>],
        _: &[Option<ColumnType>],
        _: bool,
    ) -> Result<Option<ColumnType>, SQLError> {
        Ok(None)
    }

    fn resolve_function_overload(
        &self,
        _: &str,
        _: Option<&FunctionBinding>,
        _: &[Option<String>],
        _: &[Option<ColumnType>],
        _: bool,
    ) -> Result<Option<crate::type_resolution::ResolvedFunctionOverload>, SQLError> {
        Err(error("42883", "no matching user overload".into()))
    }
}
impl RoutineResolution for InapplicableUserOverload {}

#[test]
fn named_array_options_infer_boolean_beside_inapplicable_user_overloads() {
    for sql in [
        "SELECT array_sort(value, descending => $1) FROM assignment_target",
        "SELECT array_sort(descending => $1, \"array\" => value) FROM assignment_target",
        "SELECT array_sort(nulls_first => $1, \"array\" => value, descending => $2) FROM assignment_target",
    ] {
        let plan = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0));
        let count = if sql.contains("$2") { 2 } else { 1 };
        let types = infer_prepared_parameter_types(
            &InapplicableUserOverload,
            &plan,
            &vec![None; count],
            &assignment_context(),
        ).unwrap_or_else(|error| panic!("{sql}: {error}"));
        assert_eq!(types, vec![Some(ColumnType::Boolean); count], "{sql}");
    }
}

#[test]
fn array_selection_reports_builtin_errors_and_checks_modifiers_before_input() {
    for (sql, code, message) in [
        (
            "SELECT array_sort(descending => 1, \"array\" => ARRAY[2,1])",
            "42883",
            "function array_sort(descending => integer, array => integer[]) does not exist",
        ),
        (
            "SELECT array_sort(ARRAY[2,1], 'bad') FILTER (WHERE true)",
            "42809",
            "FILTER specified, but array_sort is not an aggregate function",
        ),
        (
            "SELECT array_sort() WITHIN GROUP (ORDER BY ARRAY[2,1], true)",
            "42809",
            "WITHIN GROUP specified, but array_sort is not an aggregate function",
        ),
    ] {
        let plan = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0));
        let result = infer_prepared_parameter_types(
            &InapplicableUserOverload,
            &plan,
            &[],
            &assignment_context(),
        )
        .unwrap_err();
        assert_eq!(result.sqlstate(), Some(code), "{sql}: {result}");
        assert_eq!(result.to_string(), message, "{sql}");
    }
}

#[test]
fn declared_array_options_must_match_boolean_before_input_coercion() {
    for sql in [
        "SELECT array_sort(descending => $1, \"array\" => ARRAY[2,1])",
        "SELECT pg_catalog.array_sort(ARRAY[2,1], $1)",
    ] {
        let plan = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0));
        let result = infer_prepared_parameter_types(
            &InapplicableUserOverload,
            &plan,
            &[Some(ColumnType::Integer)],
            &assignment_context(),
        )
        .unwrap_err();
        assert_eq!(result.sqlstate(), Some("42883"), "{sql}: {result}");
    }
}
