//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Routine defaults use SQL's ordered analysis while retaining call-time evaluation.

use super::*;
use crate::plan::ExpressionPlan;

struct Aliases;
impl crate::schema::dependencies::oid_alias::OidAliasInput for Aliases {
    fn resolve_oid_alias_input(&self, _: &ColumnType, _: &str) -> Result<Option<i64>, SQLError> {
        Ok(None)
    }
}

fn expression(text: &str) -> ExpressionPlan {
    let crate::Statement::CreateFunction(mut definition) = crate::compile(&format!(
        "CREATE FUNCTION f(v integer DEFAULT {text}) RETURNS integer LANGUAGE sql AS 'SELECT v'"
    ))
    .unwrap()
    .remove(0) else {
        unreachable!()
    };
    ExpressionPlan::lower(definition.params[0].default.take().unwrap())
}

#[test]
fn default_errors_follow_expression_analysis_order() {
    for (text, state, message) in [
        (
            "absent + (SELECT 1)",
            "42703",
            "column \"absent\" does not exist",
        ),
        (
            "ARRAY[absent,$1]",
            "42703",
            "column \"absent\" does not exist",
        ),
        ("sum(absent)", "42703", "column \"absent\" does not exist"),
        (
            "count('bad'::integer)",
            "22P02",
            "invalid input syntax for type integer: \"bad\"",
        ),
        (
            "abs('bad') + absent",
            "22P02",
            "invalid input syntax for type double precision: \"bad\"",
        ),
        ("$1", "42P02", "there is no parameter $1"),
        (
            "(SELECT 1)",
            "0A000",
            "cannot use subquery in DEFAULT expression",
        ),
        (
            "count(*)",
            "42803",
            "aggregate functions are not allowed in DEFAULT expressions",
        ),
        (
            "row_number() OVER ()",
            "42P20",
            "window functions are not allowed in DEFAULT expressions",
        ),
    ] {
        let error = analyze_routine_default(
            &NoRoutines,
            &|_: &str| false,
            &Aliases,
            &mut expression(text),
            &assignment_context(),
        )
        .unwrap_err();
        assert_eq!(
            (error.sqlstate(), error.to_string()),
            (Some(state), message.into()),
            "{text}"
        );
    }
}

#[test]
fn default_analysis_reads_input_constants_without_evaluating_operators() {
    let mut plan = expression("'12'::integer / 0");
    assert_eq!(
        analyze_routine_default(
            &NoRoutines,
            &|_: &str| false,
            &Aliases,
            &mut plan,
            &assignment_context()
        )
        .unwrap(),
        Some(ColumnType::Integer)
    );
    let mut converted = false;
    plan.scalar.visit(&mut |node| {
        converted |= matches!(node, ScalarExpr::TypedLiteral { value: uqa_core::Value::Int(12), ty, .. } if ty == "integer");
    });
    assert!(converted, "the input function's value must be retained");
}
