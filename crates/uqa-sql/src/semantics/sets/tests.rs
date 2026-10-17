//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::placement::{set_valued_call_error, SetPlacement};
use super::static_setness::{builtin_call_returns_set, expression_may_return_set_statically};
use super::validation::validate_query_set_contexts;
use crate::ast::{FunctionBinding, FunctionDispatch};
use crate::plan::{QueryBlockPlan, RelationalPlan, UnifiedPlan};
use crate::{ColumnType, FunctionTypeResolver, RowSchema, SQLError};

struct NoRoutines;

impl FunctionTypeResolver for NoRoutines {
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
}

impl crate::routines::RoutineResolution for NoRoutines {}

impl crate::plan::AggregateClassifier for NoRoutines {
    fn is_registered_aggregate(&self, _: &str) -> bool {
        false
    }
}

fn builtin(name: &str) -> FunctionBinding {
    FunctionBinding {
        object_id: None,
        name: name.into(),
        argument_types: vec!["text".into(), "text".into()],
        builtin: true,
        dispatch: None,
        invocation: None,
        composite_field: None,
        resolution_error: None,
    }
}

fn block(sql: &str) -> QueryBlockPlan {
    let UnifiedPlan::Query(query) = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0))
    else {
        panic!("query")
    };
    let RelationalPlan::QueryBlock(block) = query.root else {
        panic!("query block")
    };
    *block
}

#[test]
fn placements_report_postgresql_expression_kinds() {
    for (placement, context) in [
        (SetPlacement::JoinCondition, "JOIN conditions"),
        (SetPlacement::Where, "WHERE"),
        (SetPlacement::Having, "HAVING"),
        (SetPlacement::Filter, "FILTER"),
        (SetPlacement::WindowFrame, "window definitions"),
        (SetPlacement::Update, "UPDATE"),
        (SetPlacement::Limit, "LIMIT"),
        (SetPlacement::Offset, "OFFSET"),
        (SetPlacement::Returning, "RETURNING"),
        (SetPlacement::Values, "VALUES"),
        (SetPlacement::MergeWhen, "MERGE WHEN conditions"),
        (SetPlacement::IndexExpression, "index expressions"),
        (SetPlacement::IndexPredicate, "index predicates"),
        (SetPlacement::ExecuteParameter, "EXECUTE parameters"),
        (SetPlacement::TriggerWhen, "trigger WHEN conditions"),
        (SetPlacement::CallArgument, "CALL arguments"),
    ] {
        for nested in [false, true] {
            let error = placement.admit(nested).unwrap_err();
            assert_eq!(error.sqlstate(), Some("0A000"));
            assert_eq!(
                error.to_string(),
                format!("set-returning functions are not allowed in {context}")
            );
        }
    }
    SetPlacement::Expanding.admit(true).unwrap();
    SetPlacement::FromFunction.admit(false).unwrap();
    assert_eq!(
        SetPlacement::FromFunction
            .admit(true)
            .unwrap_err()
            .to_string(),
        "set-returning functions must appear at top level of FROM"
    );
    let error = set_valued_call_error();
    assert_eq!(error.sqlstate(), Some("0A000"));
    assert_eq!(
        error.to_string(),
        "set-valued function called in context that cannot accept a set"
    );
}

#[test]
fn scalar_evaluation_recognizes_built_in_set_returning_calls() {
    assert!(builtin_call_returns_set(
        "regexp_matches",
        Some(&builtin("pg_catalog.regexp_matches"))
    ));
    assert!(builtin_call_returns_set("generate_series", None));
    assert!(builtin_call_returns_set("PG_CATALOG.UNNEST", None));
    assert!(!builtin_call_returns_set(
        "regexp_match",
        Some(&builtin("pg_catalog.regexp_match"))
    ));
    let mut user = builtin("public.regexp_matches");
    user.builtin = false;
    assert!(!builtin_call_returns_set("regexp_matches", Some(&user)));
    assert!(!builtin_call_returns_set(
        "regexp_matches",
        Some(&FunctionBinding::dispatched(FunctionDispatch::IsDistinct))
    ));
    assert!(!builtin_call_returns_set(
        "coalesce",
        Some(&FunctionBinding::polymorphic_builtin_syntax("coalesce"))
    ));
}

#[test]
fn static_classification_finds_calls_outside_subqueries() {
    for (sql, expected) in [
        ("SELECT abs(generate_series(1, 2))", true),
        ("SELECT ARRAY[regexp_matches('a', 'a')]", true),
        ("SELECT (SELECT generate_series(1, 2))", false),
        ("SELECT abs(1)", false),
    ] {
        let block = block(sql);
        assert_eq!(
            expression_may_return_set_statically(&NoRoutines, &block.projections[0].expr),
            expected,
            "{sql}"
        );
    }
}

fn validate(sql: &str) -> Result<(), SQLError> {
    validate_query_set_contexts(
        &NoRoutines,
        &NoRoutines,
        &block(sql),
        &RowSchema::default(),
        &[],
    )
}

#[test]
fn execution_validation_checks_inner_constructs_first_with_postgresql_diagnostics() {
    let hint =
        Some("You might be able to move the set-returning function into a LATERAL FROM item.");
    for (sql, sqlstate, message, expected_hint) in [
        (
            "SELECT CASE WHEN true THEN generate_series(1, 2) END",
            "0A000",
            "set-returning functions are not allowed in CASE",
            hint,
        ),
        (
            "SELECT CASE WHEN generate_series(0, 1)::boolean THEN 1 END",
            "42804",
            "argument of CASE/WHEN must not return a set",
            None,
        ),
        (
            "SELECT coalesce(generate_series(1, 2), 0)",
            "0A000",
            "set-returning functions are not allowed in COALESCE",
            hint,
        ),
        (
            "SELECT sum(CASE WHEN true THEN generate_series(1, 2) END)",
            "0A000",
            "set-returning functions are not allowed in CASE",
            hint,
        ),
        (
            "SELECT sum(generate_series(1, 2))",
            "0A000",
            "aggregate function calls cannot contain set-returning function calls",
            hint,
        ),
        (
            "SELECT count(*) FILTER (WHERE generate_series(1, 2) > 0)",
            "0A000",
            "set-returning functions are not allowed in FILTER",
            None,
        ),
        (
            "SELECT lag(generate_series(1, 2)) OVER ()",
            "0A000",
            "window function calls cannot contain set-returning function calls",
            hint,
        ),
        (
            "SELECT row_number() OVER (ROWS generate_series(1, 2) PRECEDING)",
            "0A000",
            "set-returning functions are not allowed in window definitions",
            None,
        ),
        (
            "SELECT NOT (generate_series(0, 1)::boolean)",
            "42804",
            "argument of NOT must not return a set",
            None,
        ),
        (
            "SELECT generate_series(1, 2) BETWEEN SYMMETRIC 1 AND 2",
            "42804",
            "argument of AND must not return a set",
            None,
        ),
        (
            "SELECT (generate_series(1, 2), 1) = (1, 1)",
            "42804",
            "row comparison operator must not return a set",
            None,
        ),
        (
            "SELECT 1 WHERE generate_series(1, 2) > 0",
            "0A000",
            "set-returning functions are not allowed in WHERE",
            None,
        ),
        (
            "SELECT 1 OFFSET generate_series(1, 2) LIMIT generate_series(1, 2)",
            "0A000",
            "set-returning functions are not allowed in OFFSET",
            None,
        ),
    ] {
        let error = validate(sql).expect_err(sql);
        assert_eq!(error.sqlstate(), Some(sqlstate), "{sql}");
        assert_eq!(error.to_string(), message, "{sql}");
        assert_eq!(error.hint(), expected_hint, "{sql}");
    }
}

#[test]
fn execution_validation_processes_window_keys_like_a_target_list() {
    for sql in [
        "SELECT row_number() OVER (PARTITION BY generate_series(1, 2) ORDER BY generate_series(1, 3))",
        "SELECT NOT (row_number() OVER (PARTITION BY generate_series(1, 2)) > 1)",
    ] {
        validate(sql).unwrap_or_else(|error| panic!("{sql}: {error}"));
    }
}
