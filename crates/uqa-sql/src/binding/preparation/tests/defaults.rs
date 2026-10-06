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
        let error = analyze_default_inputs(
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
        analyze_default_inputs(
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

struct RelationAliases(std::cell::RefCell<Vec<String>>);

impl crate::schema::dependencies::oid_alias::OidAliasInput for RelationAliases {
    fn resolve_oid_alias_input(
        &self,
        ty: &ColumnType,
        name: &str,
    ) -> Result<Option<i64>, SQLError> {
        assert_eq!(*ty, ColumnType::Regclass);
        self.0.borrow_mut().push(name.into());
        Ok((name == "ids").then_some(16_500))
    }
}

#[test]
fn default_sequence_inputs_store_relation_identity_without_sequence_evaluation() {
    let aliases = RelationAliases(std::cell::RefCell::new(Vec::new()));
    for text in ["nextval('ids')", "currval('ids')", "setval('ids',40)"] {
        let mut plan = expression(text);
        assert_eq!(
            analyze_default_inputs(
                &NoRoutines,
                &|_: &str| false,
                &aliases,
                &mut plan,
                &assignment_context(),
            )
            .unwrap(),
            Some(ColumnType::BigInteger)
        );
        let ScalarExpr::Func { args, .. } = plan.scalar else {
            panic!("sequence call must remain unevaluated");
        };
        assert!(matches!(
            &args[0],
            ScalarExpr::TypedLiteral {
                value: uqa_core::Value::Int(16_500),
                bound_type: Some(ColumnType::Regclass),
                ..
            }
        ));
    }
    assert_eq!(*aliases.0.borrow(), ["ids", "ids", "ids"]);
    let error = analyze_default_inputs(
        &NoRoutines,
        &|_: &str| false,
        &aliases,
        &mut expression("nextval('missing')"),
        &assignment_context(),
    )
    .unwrap_err();
    assert_eq!(error.sqlstate(), Some("42P01"));
    assert_eq!(error.to_string(), "relation \"missing\" does not exist");
}

#[test]
fn stored_default_function_coercions_preserve_late_text_and_bigint_arguments() {
    for (text, expected_type) in [
        ("nextval('missing'::text)", "regclass"),
        ("setval('ids',40)", "bigint"),
    ] {
        let aliases = RelationAliases(std::cell::RefCell::new(Vec::new()));
        let original = expression(text);
        let mut bound = original.clone();
        let binding = assignment_context();
        analyze_default_inputs(
            &NoRoutines,
            &|_: &str| false,
            &aliases,
            &mut bound,
            &binding,
        )
        .unwrap();
        crate::binding::bind_expression_plan_routines_for_storage(
            &NoRoutines,
            &mut bound,
            &[],
            &binding,
            &RowSchema::default(),
        )
        .unwrap();
        let ScalarExpr::Func { args, .. } = &bound.scalar else {
            panic!("stored sequence call");
        };
        let argument = args.last().unwrap();
        assert!(matches!(argument, ScalarExpr::Cast { ty, .. } if ty == expected_type));
        if expected_type == "regclass" {
            assert!(
                aliases.0.borrow().is_empty(),
                "typed text resolves at runtime"
            );
        }
        crate::binding::syntax_sites::expression_syntax_sites(&original, &bound).unwrap();
    }
}

#[test]
fn stored_function_coercions_keep_named_argument_positions() {
    let mut plan = expression("random(max => 2::bigint, min => 1)");
    let binding = assignment_context();
    let analyze = |plan: &mut ExpressionPlan| {
        analyze_default_inputs(&NoRoutines, &|_: &str| false, &Aliases, plan, &binding)
    };
    assert_eq!(analyze(&mut plan).unwrap(), Some(ColumnType::BigInteger));
    crate::binding::bind_expression_plan_routines_for_storage(
        &NoRoutines,
        &mut plan,
        &[],
        &binding,
        &RowSchema::default(),
    )
    .unwrap();
    assert_eq!(analyze(&mut plan).unwrap(), Some(ColumnType::BigInteger));
    let ScalarExpr::Func { args, .. } = &plan.scalar else {
        panic!("random call")
    };
    let arguments = crate::scalar_call_arguments(args).unwrap();
    assert_eq!(arguments[0].name, Some("max"));
    assert_eq!(arguments[1].name, Some("min"));
    assert!(
        matches!(arguments[1].value, ScalarExpr::Cast { expr, ty } if ty == "bigint" && matches!(expr.as_ref(), ScalarExpr::Literal(uqa_core::Value::Int(1))))
    );
}
