//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::scalar::evaluator::eval_scalar_inner;
use uqa_core::memory::ProductionControl;
use uqa_sql::ColumnType;

fn prepare(sql: &str) -> PreparedExpressions<ScalarExpr> {
    let uqa_sql::Statement::Select(mut query) = uqa_sql::compile(sql).unwrap().remove(0) else {
        panic!("SELECT");
    };
    PreparedExpressions::scalar(
        uqa_sql::plan::ExpressionPlan::lower(query.projections.remove(0).expr).scalar,
    )
}

#[test]
fn numeric_operator_rows_do_not_rebuild_type_names_and_overload_candidates() {
    let prepared = prepare("SELECT @v");
    let schema = RowSchema::with_types(vec!["v".into()], vec![Some(ColumnType::Integer)]);
    let row = crate::OwnedPhysicalRow::new(
        schema.clone(),
        crate::PhysicalRow::from_values(vec![Value::Int(-7)]),
    );
    let context = ScalarEvalContext::from_row_lookup(&row, &[])
        .with_row_schema(&schema)
        .with_function_states(prepared.calls());
    let control = ProductionControl::uncontrolled();
    let allocation = allocation_counter::measure(|| {
        for _ in 0..10_000 {
            assert_eq!(
                *eval_scalar_inner(&prepared, &context, &control).unwrap(),
                Value::Int(7),
            );
        }
    });
    assert!(allocation.bytes_total < 4 * 1024 * 1024, "{allocation:?}");
}

fn evaluate(
    expression: &PreparedExpressions<ScalarExpr>,
    schema: &RowSchema,
    value: Value,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, SQLError> {
    let row =
        crate::OwnedPhysicalRow::new(schema.clone(), crate::PhysicalRow::from_values(vec![value]));
    let context = ScalarEvalContext::from_row_lookup(&row, &[])
        .with_row_schema(schema)
        .with_function_states(expression.calls());
    eval_scalar_inner(expression, &context, control)
}

#[test]
fn numeric_operator_state_preserves_schema_and_parameter_type_changes() {
    let expression = prepare("SELECT @v");
    let integer = RowSchema::with_types(vec!["v".into()], vec![Some(ColumnType::Integer)]);
    let real = RowSchema::with_types(vec!["v".into()], vec![Some(ColumnType::Real)]);
    let control = ProductionControl::uncontrolled();
    for (schema, value, expected) in [
        (&integer, Value::Int(-7), Value::Int(7)),
        (&real, Value::Float(-1.25), Value::Float(1.25)),
        (&integer, Value::Int(-8), Value::Int(8)),
    ] {
        assert_eq!(
            *evaluate(&expression, schema, value, &control).unwrap(),
            expected
        );
    }
    let parameter = prepare("SELECT @ $1");
    assert!(parameter.calls().numeric_operators.is_empty());
    let roots = PreparedExpressions::scalars(vec![(*expression).clone(), (*parameter).clone()]);
    assert_eq!(roots.calls().numeric_operators.len(), 1);
    for (value, expected) in [
        (Value::Int(-7), Value::Int(7)),
        (Value::Float(-1.25), Value::Float(1.25)),
    ] {
        let params = [uqa_sql::SQLParam::Scalar(value)];
        let context = ScalarEvalContext::new(None, &params).with_function_states(parameter.calls());
        assert_eq!(eval_scalar(&parameter, &context).unwrap(), expected);
    }
}

#[test]
fn numeric_operator_state_is_lazy_and_failed_width_conversion_is_not_retained() {
    let expression = prepare("SELECT CASE WHEN v < 0 THEN @v ELSE 0 END");
    let schema = RowSchema::with_types(vec!["v".into()], vec![Some(ColumnType::SmallInteger)]);
    let control = ProductionControl::uncontrolled();
    assert_eq!(
        *evaluate(&expression, &schema, Value::Int(2), &control).unwrap(),
        Value::Int(0)
    );
    assert!(expression
        .calls()
        .numeric_operators
        .iter()
        .all(|(_, slot)| slot.get().is_none()));
    assert_eq!(
        evaluate(&expression, &schema, Value::Int(-32768), &control)
            .unwrap_err()
            .sqlstate(),
        Some("22003")
    );
    assert!(expression
        .calls()
        .numeric_operators
        .iter()
        .all(|(_, slot)| slot.get().is_none()));
    assert_eq!(
        *evaluate(&expression, &schema, Value::Int(-7), &control).unwrap(),
        Value::Int(7)
    );
    assert!(expression
        .calls()
        .numeric_operators
        .iter()
        .all(|(_, slot)| slot.get().is_some()));
    assert_eq!(
        evaluate(&expression, &schema, Value::Int(-32768), &control)
            .unwrap_err()
            .sqlstate(),
        Some("22003")
    );
    let cloned = expression.clone();
    assert!(cloned
        .calls()
        .numeric_operators
        .iter()
        .all(|(_, slot)| slot.get().is_none()));
}

#[test]
fn retained_numeric_operators_use_current_limits_cancellation_and_execute_privileges() {
    use std::cell::Cell;
    use uqa_core::{memory::MemoryBudget, CancellationToken};
    struct Privileges {
        calls: Cell<usize>,
        denied: Cell<bool>,
    }
    impl EngineHook for Privileges {
        fn nextval(&self, _: &str) -> Result<i64, SQLError> {
            unreachable!()
        }
        fn currval(&self, _: &str) -> Result<i64, SQLError> {
            unreachable!()
        }
        fn setval(&self, _: &str, _: i64, _: bool) -> Result<i64, SQLError> {
            unreachable!()
        }
        fn require_builtin_execute(&self, _: &FunctionBinding) -> Result<(), SQLError> {
            self.calls.set(self.calls.get() + 1);
            if self.denied.get() {
                Err(SQLError::Routine {
                    sqlstate: "42501".into(),
                    message: "permission denied for operator function".into(),
                })
            } else {
                Ok(())
            }
        }
    }
    let privileges = Privileges {
        calls: Cell::new(0),
        denied: Cell::new(false),
    };
    let expression = prepare("SELECT @v");
    let schema = RowSchema::with_types(vec!["v".into()], vec![Some(ColumnType::Integer)]);
    let row = crate::OwnedPhysicalRow::new(
        schema.clone(),
        crate::PhysicalRow::from_values(vec![Value::Int(-7)]),
    );
    let context = ScalarEvalContext::from_row_lookup(&row, &[])
        .with_row_schema(&schema)
        .with_function_states(expression.calls())
        .with_function_hook(&privileges);
    let budget = MemoryBudget::new(64 * 1024);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    for _ in 0..3 {
        assert_eq!(
            *eval_scalar_inner(&expression, &context, &control).unwrap(),
            Value::Int(7)
        );
        assert_eq!(budget.used(), 0);
    }
    assert_eq!(privileges.calls.get(), 3);
    privileges.denied.set(true);
    assert_eq!(
        eval_scalar_inner(&expression, &context, &control)
            .unwrap_err()
            .sqlstate(),
        Some("42501")
    );
    privileges.denied.set(false);
    let held = control.copy_text("still owned").unwrap();
    let occupied = budget.reserve(budget.limit() - budget.used()).unwrap();
    assert_eq!(
        eval_scalar_inner(&expression, &context, &control)
            .unwrap_err()
            .sqlstate(),
        Some("53200")
    );
    assert_eq!(&*held, "still owned");
    assert_eq!(budget.used(), held.reserved_bytes() + occupied.bytes());
    drop(occupied);
    for token in [&original, &invoking] {
        token.cancel();
        assert_eq!(
            eval_scalar_inner(&expression, &context, &control)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(budget.used(), held.reserved_bytes());
        token.reset();
    }
    drop(held);
    assert_eq!(budget.used(), 0);
}
