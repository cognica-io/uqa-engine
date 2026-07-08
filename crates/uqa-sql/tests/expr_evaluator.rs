//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Coverage for `uqa/tests/test_expr_evaluator.py::TestExprEvaluatorDirect`.
//! Drives `uqa_sql::expr::eval` against AST fragments produced by
//! `compile()` and asserts the result against per-row dictionaries.

use std::collections::BTreeMap;

use uqa_core::{DecimalValue, Value};
use uqa_sql::ast::{Expr, Statement};
use uqa_sql::expr::{eval, EvalContext};
use uqa_sql::{compile, ResultRow};

fn projection_expr(sql: &str) -> Expr {
    let stmts = compile(sql).expect("compile");
    let stmt = match stmts.into_iter().next().expect("at least one stmt") {
        Statement::Select(s) => s,
        other => panic!("expected SELECT, got {other:?}"),
    };
    stmt.projections
        .into_iter()
        .next()
        .expect("at least one projection")
        .expr
}

fn where_expr(sql: &str) -> Expr {
    let stmts = compile(sql).expect("compile");
    let stmt = match stmts.into_iter().next().expect("at least one stmt") {
        Statement::Select(s) => s,
        other => panic!("expected SELECT, got {other:?}"),
    };
    stmt.r#where.expect("expected WHERE")
}

fn row_from(pairs: &[(&str, Value)]) -> ResultRow {
    let mut r: BTreeMap<String, Value> = BTreeMap::new();
    for (k, v) in pairs {
        r.insert((*k).to_string(), v.clone());
    }
    r
}

fn dec(value: &str) -> Value {
    Value::Decimal(DecimalValue::parse(value).unwrap())
}

#[test]
fn column_ref() {
    let expr = projection_expr("SELECT x FROM t");
    let row = row_from(&[("x", Value::Int(42))]);
    let ctx = EvalContext::new(Some(&row), &[]);
    assert_eq!(eval(&expr, &ctx).unwrap(), Value::Int(42));
}

#[test]
fn const_integer() {
    let expr = projection_expr("SELECT 42 FROM t");
    let ctx = EvalContext::new(None, &[]);
    assert_eq!(eval(&expr, &ctx).unwrap(), Value::Int(42));
}

#[test]
fn const_string() {
    let expr = projection_expr("SELECT 'hello' FROM t");
    let ctx = EvalContext::new(None, &[]);
    assert_eq!(eval(&expr, &ctx).unwrap(), Value::Str("hello".into()));
}

#[test]
fn const_decimal() {
    let expr = projection_expr("SELECT 3.125 FROM t");
    let ctx = EvalContext::new(None, &[]);
    assert_eq!(eval(&expr, &ctx).unwrap(), dec("3.125"));
}

#[test]
fn mixed_float_decimal_arithmetic_promotes_to_float() {
    // PostgreSQL numeric promotion: double precision wins mixed
    // float/numeric arithmetic; exact decimal arithmetic applies only
    // when no float operand is involved.
    let add = projection_expr("SELECT x + 0.25 FROM t");
    let mul = projection_expr("SELECT x * 0.5 FROM t");

    let float_row = row_from(&[("x", Value::Float(2.0))]);
    let float_ctx = EvalContext::new(Some(&float_row), &[]);
    assert_eq!(eval(&add, &float_ctx).unwrap(), Value::Float(2.25));
    assert_eq!(eval(&mul, &float_ctx).unwrap(), Value::Float(1.0));

    let int_row = row_from(&[("x", Value::Int(2))]);
    let int_ctx = EvalContext::new(Some(&int_row), &[]);
    assert_eq!(eval(&add, &int_ctx).unwrap(), dec("2.25"));
    assert_eq!(eval(&mul, &int_ctx).unwrap(), dec("1.0"));

    let decimal_row = row_from(&[("x", dec("1.5"))]);
    let decimal_ctx = EvalContext::new(Some(&decimal_row), &[]);
    assert_eq!(eval(&add, &decimal_ctx).unwrap(), dec("1.75"));
    assert_eq!(eval(&mul, &decimal_ctx).unwrap(), dec("0.75"));
}

#[test]
fn bool_and() {
    let expr = where_expr("SELECT * FROM t WHERE x > 5 AND y < 10");
    let row1 = row_from(&[("x", Value::Int(10)), ("y", Value::Int(3))]);
    let ctx1 = EvalContext::new(Some(&row1), &[]);
    assert_eq!(eval(&expr, &ctx1).unwrap(), Value::Bool(true));

    let row2 = row_from(&[("x", Value::Int(10)), ("y", Value::Int(20))]);
    let ctx2 = EvalContext::new(Some(&row2), &[]);
    assert_eq!(eval(&expr, &ctx2).unwrap(), Value::Bool(false));
}

#[test]
fn bool_or() {
    let expr = where_expr("SELECT * FROM t WHERE x > 100 OR y < 10");
    let row1 = row_from(&[("x", Value::Int(1)), ("y", Value::Int(3))]);
    let ctx1 = EvalContext::new(Some(&row1), &[]);
    assert_eq!(eval(&expr, &ctx1).unwrap(), Value::Bool(true));

    let row2 = row_from(&[("x", Value::Int(1)), ("y", Value::Int(20))]);
    let ctx2 = EvalContext::new(Some(&row2), &[]);
    assert_eq!(eval(&expr, &ctx2).unwrap(), Value::Bool(false));
}

#[test]
fn bool_not() {
    let expr = where_expr("SELECT * FROM t WHERE NOT x > 5");
    let row1 = row_from(&[("x", Value::Int(3))]);
    let ctx1 = EvalContext::new(Some(&row1), &[]);
    assert_eq!(eval(&expr, &ctx1).unwrap(), Value::Bool(true));

    let row2 = row_from(&[("x", Value::Int(10))]);
    let ctx2 = EvalContext::new(Some(&row2), &[]);
    assert_eq!(eval(&expr, &ctx2).unwrap(), Value::Bool(false));
}

#[test]
fn in_expr() {
    let expr = where_expr("SELECT * FROM t WHERE x IN (1, 2, 3)");
    let row1 = row_from(&[("x", Value::Int(2))]);
    let ctx1 = EvalContext::new(Some(&row1), &[]);
    assert_eq!(eval(&expr, &ctx1).unwrap(), Value::Bool(true));

    let row2 = row_from(&[("x", Value::Int(5))]);
    let ctx2 = EvalContext::new(Some(&row2), &[]);
    assert_eq!(eval(&expr, &ctx2).unwrap(), Value::Bool(false));
}

#[test]
fn between() {
    let expr = where_expr("SELECT * FROM t WHERE x BETWEEN 10 AND 20");
    let row1 = row_from(&[("x", Value::Int(15))]);
    let ctx1 = EvalContext::new(Some(&row1), &[]);
    assert_eq!(eval(&expr, &ctx1).unwrap(), Value::Bool(true));

    let row2 = row_from(&[("x", Value::Int(25))]);
    let ctx2 = EvalContext::new(Some(&row2), &[]);
    assert_eq!(eval(&expr, &ctx2).unwrap(), Value::Bool(false));
}

#[test]
fn not_between() {
    let expr = where_expr("SELECT * FROM t WHERE x NOT BETWEEN 10 AND 20");
    let row1 = row_from(&[("x", Value::Int(25))]);
    let ctx1 = EvalContext::new(Some(&row1), &[]);
    assert_eq!(eval(&expr, &ctx1).unwrap(), Value::Bool(true));

    let row2 = row_from(&[("x", Value::Int(15))]);
    let ctx2 = EvalContext::new(Some(&row2), &[]);
    assert_eq!(eval(&expr, &ctx2).unwrap(), Value::Bool(false));
}

#[test]
fn typeof_function() {
    let expr = projection_expr("SELECT typeof(x) FROM t");
    for (val, expected) in [
        (Value::Int(42), "integer"),
        (Value::Float(3.125), "double precision"),
        (dec("3.125"), "numeric"),
        (Value::Str("hello".into()), "text"),
        (Value::Null, "null"),
    ] {
        let row = row_from(&[("x", val)]);
        let ctx = EvalContext::new(Some(&row), &[]);
        let got = eval(&expr, &ctx).unwrap();
        assert_eq!(got, Value::Str(expected.into()));
    }
}

#[test]
fn unsupported_function() {
    let expr = projection_expr("SELECT pg_sleep(1) FROM t");
    let ctx = EvalContext::new(None, &[]);
    let r = eval(&expr, &ctx);
    assert!(r.is_err());
}

#[test]
fn unknown_column_returns_null() {
    // Mirrors the canonical UQA implementation's behaviour for an unknown ColumnRef in scalar
    // evaluation: returns Null rather than panicking.
    let expr = projection_expr("SELECT y FROM t");
    let row = row_from(&[("x", Value::Int(1))]);
    let ctx = EvalContext::new(Some(&row), &[]);
    assert_eq!(eval(&expr, &ctx).unwrap(), Value::Null);
}

#[test]
fn nested_arithmetic_in_projection() {
    let expr = projection_expr("SELECT (x + 1) * 2 FROM t");
    let row = row_from(&[("x", Value::Int(3))]);
    let ctx = EvalContext::new(Some(&row), &[]);
    assert_eq!(eval(&expr, &ctx).unwrap(), Value::Int(8));
}
