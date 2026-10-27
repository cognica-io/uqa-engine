//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

mod scoped;
use crate::scalar::evaluator::eval_scalar_inner;
use std::cell::Cell;
use uqa_core::{
    memory::{MemoryBudget, ProductionControl},
    CancellationToken,
};
use uqa_sql::{
    ast::{RoutineInvocationBinding, RoutineVariadicMode},
    ColumnType,
};

#[derive(Default)]
struct LiveCatalog {
    narrow: Cell<bool>,
    fail: Cell<bool>,
    resolutions: Cell<usize>,
    callbacks: Cell<usize>,
    calls: Cell<usize>,
}

impl EngineHook for LiveCatalog {
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
        Ok(if name == "chosen" {
            Some(if self.narrow.get() {
                ColumnType::SmallInteger
            } else {
                ColumnType::Integer
            })
        } else {
            ColumnType::from_sql_name(name).ok()
        })
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
                message: "current constraint".into(),
            })
        } else {
            Ok(None)
        }
    }
    fn call_bound_user_function(
        &self,
        binding: &FunctionBinding,
        args: &[(Option<String>, Value)],
    ) -> Option<Result<Value, SQLError>> {
        assert_eq!(binding.object_id, Some([7; 16]));
        assert!(args.is_empty());
        self.calls.set(self.calls.get() + 1);
        Some(Ok(Value::Int(i64::try_from(self.calls.get()).unwrap())))
    }
}

fn prepare(operand: ScalarExpr, target: &str) -> PreparedExpressions<ScalarExpr> {
    PreparedExpressions::scalar(ScalarExpr::Cast {
        expr: Box::new(operand),
        ty: target.into(),
        implicit: false,
    })
}

fn evaluate(
    prepared: &PreparedExpressions<ScalarExpr>,
    schema: &RowSchema,
    value: Value,
    catalog: &dyn EngineHook,
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
fn catalog_casts_reuse_source_inference_with_live_resolution_and_constraints() {
    let prepared = prepare(ScalarExpr::Column("v".into()), "chosen");
    let schema = RowSchema::with_types(vec!["v".into()], vec![Some(ColumnType::Integer)]);
    let catalog = LiveCatalog::default();
    let control = ProductionControl::uncontrolled();
    CAST_TYPE_RESOLUTIONS.with(|count| count.set(0));
    for value in 0..1_000 {
        catalog.narrow.set(value % 2 == 0);
        assert_eq!(
            *evaluate(&prepared, &schema, Value::Int(value), &catalog, &control).unwrap(),
            Value::Int(value)
        );
    }
    assert_eq!(catalog.resolutions.get(), 2_000);
    assert_eq!(catalog.callbacks.get(), 1_000);
    catalog.narrow.set(true);
    assert_eq!(
        evaluate(&prepared, &schema, Value::Int(32768), &catalog, &control)
            .unwrap_err()
            .sqlstate(),
        Some("22003")
    );
    catalog.narrow.set(false);
    assert_eq!(
        *evaluate(&prepared, &schema, Value::Int(32768), &catalog, &control).unwrap(),
        Value::Int(32768)
    );
    catalog.fail.set(true);
    assert_eq!(
        evaluate(&prepared, &schema, Value::Int(1), &catalog, &control)
            .unwrap_err()
            .sqlstate(),
        Some("23514")
    );
    assert_eq!(CAST_TYPE_RESOLUTIONS.with(Cell::get), 1);
}

#[test]
fn routine_cast_source_metadata_never_reuses_routine_results_or_type_resolution() {
    let routine = ScalarExpr::Func {
        name: "produce_value".into(),
        binding: Some(FunctionBinding {
            object_id: Some([7; 16]),
            name: "produce_value".into(),
            argument_types: vec![],
            builtin: false,
            dispatch: None,
            invocation: Some(Box::new(RoutineInvocationBinding {
                argument_positions: vec![],
                argument_targets: vec![],
                argument_sources: vec![],
                parameter_types: vec![],
                return_type: Some("chosen".into()),
                variadic_mode: RoutineVariadicMode::default(),
            })),
            value_binding: None,
            resolution_error: None,
            input_collation: None,
        }),
        args: vec![],
        distinct: false,
        order_by: vec![],
        order_syntax: FunctionOrderSyntax::default(),
        filter: None,
    };
    let prepared = prepare(routine, "pg_catalog.int8");
    let catalog = LiveCatalog::default();
    let schema = RowSchema::default();
    let control = ProductionControl::uncontrolled();
    CAST_TYPE_RESOLUTIONS.with(|count| count.set(0));
    for value in 1..=1_000 {
        catalog.narrow.set(value % 2 == 0);
        assert_eq!(
            *evaluate(&prepared, &schema, Value::Null, &catalog, &control).unwrap(),
            Value::Int(value)
        );
    }
    assert_eq!(catalog.calls.get(), 1_000);
    assert_eq!(catalog.callbacks.get(), 1_000);
    assert_eq!(catalog.resolutions.get(), 2_000);
    assert_eq!(CAST_TYPE_RESOLUTIONS.with(Cell::get), 1);
}

#[test]
fn catalog_cast_source_retention_preserves_schema_identity_errors_and_owned_memory() {
    let prepared = prepare(ScalarExpr::Column("v".into()), "chosen");
    let integer = RowSchema::with_types(vec!["v".into()], vec![Some(ColumnType::Integer)]);
    let real = RowSchema::with_types(vec!["v".into()], vec![Some(ColumnType::DoublePrecision)]);
    let catalog = LiveCatalog::default();
    let budget = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    catalog.fail.set(true);
    assert_eq!(
        evaluate(&prepared, &integer, Value::Int(1), &catalog, &control)
            .unwrap_err()
            .sqlstate(),
        Some("23514")
    );
    assert_eq!(budget.used(), 0);
    assert!(prepared.calls().row_casts[0].1.get().is_none());
    catalog.fail.set(false);
    CAST_TYPE_RESOLUTIONS.with(|count| count.set(0));
    for (schema, input, expected) in [
        (&integer, Value::Int(7), Value::Int(7)),
        (&real, Value::Float(2.5), Value::Int(2)),
        (&integer, Value::Int(8), Value::Int(8)),
    ] {
        assert_eq!(
            *evaluate(&prepared, schema, input, &catalog, &control).unwrap(),
            expected
        );
    }
    assert_eq!(CAST_TYPE_RESOLUTIONS.with(Cell::get), 2);
    assert!(budget.used() > 0 && budget.peak() <= 4096);
    let RowCast::Source {
        source: Some(source),
        ..
    } = prepared.calls().row_casts[0].1.get().unwrap()
    else {
        panic!("catalog casts retain only their inferred source name");
    };
    assert_eq!(budget.used(), source.reserved_bytes());
    let observed = (catalog.resolutions.get(), catalog.callbacks.get());
    token.cancel();
    assert_eq!(
        evaluate(&prepared, &integer, Value::Null, &catalog, &control)
            .unwrap_err()
            .sqlstate(),
        Some("57014")
    );
    assert_eq!(
        observed,
        (catalog.resolutions.get(), catalog.callbacks.get())
    );
    drop(prepared);
    assert_eq!(budget.used(), 0);
}

#[test]
fn unknown_cast_sources_keep_per_value_conversion_without_reinferring_schema() {
    let prepared = prepare(ScalarExpr::Column("v".into()), "chosen");
    let schema = RowSchema::with_types(vec!["v".into()], vec![None]);
    let catalog = LiveCatalog::default();
    let control = ProductionControl::uncontrolled();
    CAST_TYPE_RESOLUTIONS.with(|count| count.set(0));
    for (input, expected) in [
        (Value::Int(1), Value::Int(1)),
        (Value::Float(2.5), Value::Int(2)),
        (Value::Str("3".into()), Value::Int(3)),
        (Value::Null, Value::Null),
    ] {
        assert_eq!(
            *evaluate(&prepared, &schema, input, &catalog, &control).unwrap(),
            expected
        );
    }
    assert!(matches!(
        prepared.calls().row_casts[0].1.get(),
        Some(RowCast::Source { source: None, .. })
    ));
    assert_eq!(CAST_TYPE_RESOLUTIONS.with(Cell::get), 1);
    assert_eq!(catalog.resolutions.get(), 4);
    assert_eq!(catalog.callbacks.get(), 4);
}
