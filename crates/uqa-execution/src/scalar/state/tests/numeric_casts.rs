//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::scalar::evaluator::eval_scalar_inner;
use std::cell::Cell;
use uqa_core::{
    memory::{MemoryBudget, ProductionControl},
    CancellationToken,
};
use uqa_sql::ColumnType;

#[derive(Default)]
struct NumericCatalog {
    resolutions: Cell<usize>,
    callbacks: Cell<usize>,
    fail: Cell<bool>,
}

impl EngineHook for NumericCatalog {
    fn nextval(&self, _: &str) -> Result<i64, SQLError> {
        unreachable!()
    }
    fn currval(&self, _: &str) -> Result<i64, SQLError> {
        unreachable!()
    }
    fn setval(&self, _: &str, _: i64, _: bool) -> Result<i64, SQLError> {
        unreachable!()
    }
    fn resolve_type_name(&self, name: &str) -> Result<Option<ColumnType>, String> {
        self.resolutions.set(self.resolutions.get() + 1);
        Ok(ColumnType::from_sql_name(name).ok())
    }
    fn cast_domain(
        &self,
        _: &Value,
        _: Option<&str>,
        _: &ColumnType,
    ) -> Result<Option<Value>, SQLError> {
        self.callbacks.set(self.callbacks.get() + 1);
        if self.fail.get() {
            Err(SQLError::Routine {
                sqlstate: "23514".into(),
                message: "invoking domain adapter".into(),
            })
        } else {
            Ok(None)
        }
    }
}

fn prepared(target: &str) -> PreparedExpressions<ScalarExpr> {
    PreparedExpressions::scalar(ScalarExpr::Cast {
        expr: Box::new(ScalarExpr::Column("v".into())),
        ty: target.into(),
        implicit: false,
    })
}

fn evaluate(
    prepared: &PreparedExpressions<ScalarExpr>,
    schema: &RowSchema,
    value: Value,
    catalog: &NumericCatalog,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, SQLError> {
    let row =
        crate::OwnedPhysicalRow::new(schema.clone(), crate::PhysicalRow::from_values(vec![value]));
    let context = ScalarEvalContext::from_row_lookup(&row, &[])
        .with_row_schema(schema)
        .with_function_states(prepared.calls())
        .with_function_hook(catalog);
    eval_scalar_inner(prepared, &context, control)
}

#[test]
fn numeric_row_casts_resolve_types_once_and_retain_only_the_source_name() {
    let prepared = prepared("pg_catalog.int8");
    let schema = RowSchema::with_types(vec!["v".into()], vec![Some(ColumnType::Integer)]);
    let catalog = NumericCatalog::default();
    let budget = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    CAST_TYPE_RESOLUTIONS.with(|count| count.set(0));
    for value in 0..10_000 {
        assert_eq!(
            *evaluate(&prepared, &schema, Value::Int(value), &catalog, &control).unwrap(),
            Value::Int(value)
        );
    }
    assert_eq!(CAST_TYPE_RESOLUTIONS.with(Cell::get), 1);
    assert_eq!(catalog.resolutions.get(), 2);
    assert_eq!(catalog.callbacks.get(), 10_000);
    assert_eq!(prepared.calls().row_casts.len(), 1);
    let RowCast::Resolved { source, .. } = prepared.calls().row_casts[0].1.get().unwrap() else {
        panic!("numeric state");
    };
    assert_eq!(budget.used(), source.reserved_bytes());
    assert!(budget.used() > 0 && budget.peak() <= 4096);

    let clone = prepared.clone();
    assert_eq!(
        *evaluate(&clone, &schema, Value::Int(7), &catalog, &control).unwrap(),
        Value::Int(7)
    );
    assert_eq!(catalog.resolutions.get(), 4);
    drop(clone);
    drop(prepared);
    assert_eq!(budget.used(), 0);
}

#[test]
fn retained_numeric_casts_keep_current_callbacks_limits_cancellation_and_errors() {
    let prepared = prepared("pg_catalog.int2");
    let schema = RowSchema::with_types(vec!["v".into()], vec![Some(ColumnType::Integer)]);
    let catalog = NumericCatalog::default();
    let budget = MemoryBudget::new(4096);
    let first = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &first, &invoking);
    assert_eq!(
        *evaluate(&prepared, &schema, Value::Int(1), &catalog, &control).unwrap(),
        Value::Int(1)
    );
    let scratch = MemoryBudget::new(0);
    let limited = ProductionControl::new(&scratch, &first, &invoking);
    assert_eq!(
        *evaluate(&prepared, &schema, Value::Int(2), &catalog, &limited).unwrap(),
        Value::Int(2)
    );
    assert_eq!(
        *evaluate(&prepared, &schema, Value::Null, &catalog, &limited).unwrap(),
        Value::Null
    );
    assert_eq!(
        evaluate(&prepared, &schema, Value::Int(32768), &catalog, &limited)
            .unwrap_err()
            .sqlstate(),
        Some("22003")
    );
    catalog.fail.set(true);
    assert_eq!(
        evaluate(&prepared, &schema, Value::Int(2), &catalog, &limited)
            .unwrap_err()
            .sqlstate(),
        Some("23514")
    );
    assert_eq!(catalog.resolutions.get(), 2);
    invoking.cancel();
    let callbacks = catalog.callbacks.get();
    assert_eq!(
        evaluate(&prepared, &schema, Value::Int(2), &catalog, &limited)
            .unwrap_err()
            .sqlstate(),
        Some("57014")
    );
    assert_eq!(catalog.callbacks.get(), callbacks);
    assert_eq!(scratch.peak(), 0);
    drop(prepared);
    assert_eq!(budget.used(), 0);
}

#[test]
fn numeric_cast_descriptors_do_not_cross_input_schema_identity() {
    let prepared = prepared("pg_catalog.int4");
    let integer = RowSchema::with_types(vec!["v".into()], vec![Some(ColumnType::Integer)]);
    let real = RowSchema::with_types(vec!["v".into()], vec![Some(ColumnType::DoublePrecision)]);
    let catalog = NumericCatalog::default();
    let control = ProductionControl::uncontrolled();
    assert_eq!(
        *evaluate(&prepared, &integer, Value::Int(7), &catalog, &control).unwrap(),
        Value::Int(7)
    );
    assert_eq!(
        *evaluate(&prepared, &real, Value::Float(2.5), &catalog, &control).unwrap(),
        Value::Int(2)
    );
    assert_eq!(catalog.resolutions.get(), 4);
    assert_eq!(
        *evaluate(&prepared, &integer, Value::Int(8), &catalog, &control).unwrap(),
        Value::Int(8)
    );
    assert_eq!(catalog.resolutions.get(), 4);
}

#[test]
fn row_cast_state_preserves_unqualified_targets_and_parameter_type_changes() {
    let prepared = prepared("int4");
    let schema = RowSchema::with_types(vec!["v".into()], vec![Some(ColumnType::Integer)]);
    let catalog = NumericCatalog::default();
    let control = ProductionControl::uncontrolled();
    for value in 0..3 {
        evaluate(&prepared, &schema, Value::Int(value), &catalog, &control).unwrap();
    }
    assert_eq!(catalog.resolutions.get(), 6);
    assert!(matches!(
        prepared.calls().row_casts[0].1.get(),
        Some(RowCast::Source { .. })
    ));
    let nested = PreparedExpressions::scalar(ScalarExpr::Cast {
        expr: Box::new(ScalarExpr::Cast {
            expr: Box::new(ScalarExpr::Column("v".into())),
            ty: "int4".into(),
            implicit: false,
        }),
        ty: "pg_catalog.int8".into(),
        implicit: false,
    });
    let before = catalog.resolutions.get();
    for value in 0..3 {
        evaluate(&nested, &schema, Value::Int(value), &catalog, &control).unwrap();
    }
    assert_eq!(catalog.resolutions.get() - before, 12);
    assert!(nested
        .calls()
        .row_casts
        .iter()
        .all(|(_, state)| matches!(state.get(), Some(RowCast::Source { .. }))));
    let parameter = PreparedExpressions::scalar(ScalarExpr::Cast {
        expr: Box::new(ScalarExpr::Param(1)),
        ty: "pg_catalog.int4".into(),
        implicit: false,
    });
    assert!(parameter.calls().row_casts.is_empty());
    let independent = PreparedExpressions::scalars(vec![(*prepared).clone(), (*parameter).clone()]);
    assert_eq!(independent.calls().row_casts.len(), 1);
    for (parameter_value, expected) in [
        (uqa_sql::SQLParam::Scalar(Value::Int(7)), Value::Int(7)),
        (uqa_sql::SQLParam::Scalar(Value::Float(2.5)), Value::Int(2)),
    ] {
        let params = [parameter_value];
        let context = ScalarEvalContext::new(None, &params)
            .with_function_states(parameter.calls())
            .with_function_hook(&catalog);
        assert_eq!(eval_scalar(&parameter, &context).unwrap(), expected);
    }
}

#[test]
fn numeric_descriptors_are_lazy_and_do_not_retain_failed_conversions() {
    let uqa_sql::Statement::Select(query) =
        uqa_sql::compile("SELECT CASE WHEN v < 0 THEN v::smallint ELSE 0 END")
            .unwrap()
            .remove(0)
    else {
        panic!("SELECT");
    };
    let expression = uqa_sql::plan::ExpressionPlan::lower(query.projections[0].expr.clone()).scalar;
    let prepared = PreparedExpressions::scalar(expression);
    let schema = RowSchema::with_types(vec!["v".into()], vec![Some(ColumnType::Integer)]);
    let catalog = NumericCatalog::default();
    let control = ProductionControl::uncontrolled();
    assert_eq!(
        *evaluate(&prepared, &schema, Value::Int(32768), &catalog, &control).unwrap(),
        Value::Int(0)
    );
    assert!(prepared
        .calls()
        .row_casts
        .iter()
        .all(|(_, slot)| slot.get().is_none()));
    assert_eq!(catalog.callbacks.get(), 0);
    assert_eq!(
        evaluate(&prepared, &schema, Value::Int(-32769), &catalog, &control)
            .unwrap_err()
            .sqlstate(),
        Some("22003")
    );
    assert!(prepared
        .calls()
        .row_casts
        .iter()
        .all(|(_, slot)| slot.get().is_none()));
    assert_eq!(
        *evaluate(&prepared, &schema, Value::Int(-1), &catalog, &control).unwrap(),
        Value::Int(-1)
    );
    let callbacks = catalog.callbacks.get();
    assert_eq!(
        *evaluate(&prepared, &schema, Value::Int(32768), &catalog, &control).unwrap(),
        Value::Int(0)
    );
    assert_eq!(catalog.callbacks.get(), callbacks);
}
