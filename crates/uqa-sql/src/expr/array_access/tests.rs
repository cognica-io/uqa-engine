//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::{Expr, Statement};
use uqa_core::{memory::MemoryBudget, ArrayValue, CancellationToken};

const DEFAULT_COLLATION: u32 = crate::catalog::collations::DEFAULT_COLLATION_OID as u32;

fn syntax(sql: &str) -> (Expr, ArrayAccess) {
    let Statement::Select(mut query) = crate::compile(sql).unwrap().remove(0) else {
        panic!("SELECT expression");
    };
    let expression = query.projections.remove(0).expr;
    let Expr::Func {
        binding: Some(binding),
        args,
        ..
    } = &expression
    else {
        panic!("array syntax");
    };
    let access = ArrayAccess::from_dispatch(binding.dispatch.unwrap(), args.len(), |index| {
        matches!(args[index], Expr::Literal(Value::Null))
    })
    .unwrap()
    .unwrap();
    (expression, access)
}

fn values() -> Value {
    Value::Array(ArrayValue::try_new(vec![Value::Int(1), Value::Int(2), Value::Int(3)]).unwrap())
}

#[test]
fn omitted_bounds_survive_storage_without_conflating_supplied_null() {
    let (omitted, _) = syntax("SELECT (ARRAY[1,2,3])[:2]");
    let (supplied, _) = syntax("SELECT (ARRAY[1,2,3])[NULL:2]");
    assert_ne!(omitted, supplied);
    for expression in [omitted, supplied] {
        let encoded = serde_json::to_vec(&expression).unwrap();
        let decoded: Expr = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(decoded, expression);
        let budget = MemoryBudget::new(4096);
        let token = CancellationToken::new();
        let copied =
            crate::plan::ExpressionPlan::lower_column_budgeted(&decoded, &budget, &token, &token)
                .unwrap();
        assert_eq!(
            *copied,
            crate::plan::ExpressionPlan::lower(expression).scalar
        );
        drop(copied);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn legacy_slice_upgrade_retains_supplied_expressions_before_constant_folding() {
    for (sql, omitted) in [
        ("SELECT a[:2]", true),
        ("SELECT a[NULL::integer:2]", false),
        (
            "SELECT a[CASE WHEN true THEN NULL::integer ELSE 1 END:2]",
            false,
        ),
    ] {
        let (mut expression, _) = syntax(sql);
        let Expr::Func {
            binding: Some(binding),
            ..
        } = &mut expression
        else {
            unreachable!()
        };
        binding.dispatch = Some(crate::ast::FunctionDispatch::ArraySlices);
        let encoded = serde_json::to_vec(&expression).unwrap();
        let mut restored: Expr = serde_json::from_slice(&encoded).unwrap();
        assert!(restored.upgrade_legacy_serialized_dispatches());
        assert!(!restored.upgrade_legacy_serialized_dispatches());
        let Expr::Func {
            binding: Some(binding),
            ..
        } = restored
        else {
            unreachable!()
        };
        let Some(crate::ast::FunctionDispatch::ArraySlicesWithBounds(bounds)) = binding.dispatch
        else {
            panic!("upgraded array syntax")
        };
        assert_eq!(bounds.is_omitted(0, true), omitted, "{sql}");
        assert!(!bounds.is_omitted(0, false));
    }
}

#[test]
fn slice_values_match_null_absence_and_integer_rounding_references() {
    let token = CancellationToken::new();
    let budget = MemoryBudget::new(4096);
    let control = ProductionControl::new(&budget, &token, &token);
    for (sql, bounds, expected) in [
        (
            "SELECT a[:2]",
            [Value::Null, Value::Int(2)],
            Value::Array(ArrayValue::try_new(vec![Value::Int(1), Value::Int(2)]).unwrap()),
        ),
        (
            "SELECT a[NULL:2]",
            [Value::Null, Value::Int(2)],
            Value::Null,
        ),
        (
            "SELECT a[1:NULL]",
            [Value::Int(1), Value::Null],
            Value::Null,
        ),
        (
            "SELECT a[1:2.8]",
            [Value::Int(1), Value::Float(2.8)],
            values(),
        ),
    ] {
        let (_, access) = syntax(sql);
        let input = [values(), bounds[0].clone(), bounds[1].clone()];
        let output = eval_array_access_with_control(access, &control, |index| {
            Ok(control.copy_value(&input[index])?)
        })
        .unwrap();
        assert_eq!(*output, expected, "{sql}");
        assert_eq!(budget.used(), output.reserved_bytes());
        drop(output);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn pre_evaluated_array_bounds_coerce_without_copying_unselected_input() {
    let input = Value::Array(ArrayValue::try_new((1..=4096).map(Value::Int).collect()).unwrap());
    let budget = MemoryBudget::new(512);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    for (sql, bounds, expected) in [
        ("SELECT a[2.8]", vec![Value::Float(2.8)], Value::Int(3)),
        (
            "SELECT a[:2.8]",
            vec![Value::Null, Value::Float(2.8)],
            values(),
        ),
        (
            "SELECT a[NULL:2.8]",
            vec![Value::Null, Value::Float(2.8)],
            Value::Null,
        ),
    ] {
        let (
            Expr::Func {
                binding: Some(binding),
                ..
            },
            _,
        ) = syntax(sql)
        else {
            unreachable!()
        };
        let mut arguments = vec![input.clone()];
        arguments.extend(bounds);
        let result = crate::expr::scalar_postgres::eval_dispatched_postgres_function_with_control(
            binding.dispatch.unwrap(),
            &arguments,
            DEFAULT_COLLATION,
            &control,
        )
        .unwrap()
        .unwrap();
        assert_eq!(*result, expected, "{sql}");
        assert_eq!(budget.used(), result.reserved_bytes());
        drop(result);
        assert_eq!(budget.used(), 0);
    }
    let (
        Expr::Func {
            binding: Some(binding),
            ..
        },
        _,
    ) = syntax("SELECT a[:1]")
    else {
        unreachable!()
    };
    for (upper, state) in [
        (Value::Str("invalid".into()), "22P02"),
        (Value::Int(i64::MAX), "22003"),
    ] {
        let error = crate::expr::scalar_postgres::eval_dispatched_postgres_function_with_control(
            binding.dispatch.unwrap(),
            &[input.clone(), Value::Null, upper],
            DEFAULT_COLLATION,
            &control,
        )
        .unwrap()
        .unwrap_err();
        assert_eq!(error.sqlstate(), Some(state));
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn array_access_preserves_null_array_prefix_and_upper_before_lower_effects() {
    let budget = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let (_, access) = syntax("SELECT a[lo:hi][lo2:hi2]");
    for (array, expected) in [(Value::Null, vec![0]), (values(), vec![0, 2, 4, 1, 3])] {
        let mut trace = Vec::new();
        let result = eval_array_access_with_control(access, &control, |index| {
            trace.push(index);
            Ok(control.copy_value(if index == 0 { &array } else { &Value::Null })?)
        })
        .unwrap();
        assert_eq!(*result, Value::Null);
        assert_eq!(trace, expected);
        drop(result);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn array_access_releases_admitted_inputs_on_cancellation_and_quota_failure() {
    let (_, access) = syntax("SELECT a[1:2]");
    for cancelled in [false, true] {
        let budget = MemoryBudget::new(if cancelled { 4096 } else { 0 });
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let input = values();
        let result = eval_array_access_with_control(access, &control, |index| {
            if index > 0 {
                token.cancel();
            }
            Ok(control.copy_value(if index == 0 { &input } else { &Value::Int(1) })?)
        });
        assert!(result.is_err());
        assert_eq!(budget.used(), 0);
    }
}
