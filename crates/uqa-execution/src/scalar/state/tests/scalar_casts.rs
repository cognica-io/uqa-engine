//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::cell::Cell;
use uqa_sql::ColumnType;

mod temporal;

#[derive(Default)]
struct ScalarCatalog {
    resolutions: Cell<usize>,
    callbacks: Cell<usize>,
    fail: Cell<bool>,
}

impl EngineHook for ScalarCatalog {
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
                message: "current scalar cast adapter".into(),
            })
        } else {
            Ok(None)
        }
    }
}

#[test]
fn retained_scalar_casts_keep_current_callbacks_and_admitted_output() {
    use uqa_core::{
        memory::{MemoryBudget, ProductionControl},
        CancellationToken,
    };
    let prepared = PreparedExpressions::scalar(ScalarExpr::Cast {
        expr: Box::new(ScalarExpr::Column("v".into())),
        ty: "pg_catalog.varchar(4)".into(),
        implicit: false,
    });
    let schema = RowSchema::with_types(vec!["v".into()], vec![Some(ColumnType::Real)]);
    let row = crate::OwnedPhysicalRow::new(
        schema.clone(),
        crate::PhysicalRow::from_values(vec![Value::Float(1.234_567)]),
    );
    let catalog = ScalarCatalog::default();
    let context = ScalarEvalContext::from_row_lookup(&row, &[])
        .with_row_schema(&schema)
        .with_function_states(prepared.calls())
        .with_function_hook(&catalog);
    let token = CancellationToken::new();
    let invoking = CancellationToken::new();
    let budget = MemoryBudget::new(4096);
    let control = ProductionControl::new(&budget, &token, &invoking);
    let evaluate = |control: &ProductionControl<'_>| {
        crate::scalar::evaluator::eval_scalar_inner(&prepared, &context, control)
    };
    assert_eq!(*evaluate(&control).unwrap(), Value::Str("1.23".into()));
    let retained = budget.used();
    assert!(retained > 0);
    for _ in 0..1000 {
        assert_eq!(*evaluate(&control).unwrap(), Value::Str("1.23".into()));
        assert_eq!(budget.used(), retained);
    }
    assert_eq!(catalog.resolutions.get(), 2);
    assert_eq!(catalog.callbacks.get(), 1001);
    let scratch = MemoryBudget::new(0);
    let limited = ProductionControl::new(&scratch, &token, &invoking);
    assert_eq!(evaluate(&limited).unwrap_err().sqlstate(), Some("53200"));
    catalog.fail.set(true);
    assert_eq!(evaluate(&limited).unwrap_err().sqlstate(), Some("23514"));
    let other_row = crate::OwnedPhysicalRow::new(
        schema.clone(),
        crate::PhysicalRow::from_values(vec![Value::List(Vec::new())]),
    );
    let other_context = ScalarEvalContext::from_row_lookup(&other_row, &[])
        .with_row_schema(&schema)
        .with_function_states(prepared.calls())
        .with_function_hook(&catalog);
    assert_eq!(
        crate::scalar::evaluator::eval_scalar_inner(&prepared, &other_context, &limited)
            .unwrap_err()
            .sqlstate(),
        Some("23514"),
        "a different runtime carrier keeps the generic callback before output admission"
    );
    invoking.cancel();
    let callbacks = catalog.callbacks.get();
    assert_eq!(evaluate(&control).unwrap_err().sqlstate(), Some("57014"));
    assert_eq!(catalog.callbacks.get(), callbacks);
    assert_eq!(scratch.used(), 0);
    drop(prepared);
    assert_eq!(budget.used(), 0);
}

#[test]
fn scalar_row_casts_resolve_types_once_without_reusing_row_values() {
    let prepared = PreparedExpressions::scalar(ScalarExpr::Cast {
        expr: Box::new(ScalarExpr::Column("v".into())),
        ty: "pg_catalog.varchar(12)".into(),
        implicit: false,
    });
    let schema = RowSchema::with_types(vec!["v".into()], vec![Some(ColumnType::Real)]);
    let catalog = ScalarCatalog::default();
    CAST_TYPE_RESOLUTIONS.with(|count| count.set(0));
    for index in 0..10_000 {
        let value = if index % 2 == 0 { 1.234_567_89 } else { 2.5 };
        let row = crate::OwnedPhysicalRow::new(
            schema.clone(),
            crate::PhysicalRow::from_values(vec![Value::Float(value)]),
        );
        let context = ScalarEvalContext::from_row_lookup(&row, &[])
            .with_row_schema(&schema)
            .with_function_states(prepared.calls())
            .with_function_hook(&catalog);
        assert_eq!(
            eval_scalar(&prepared, &context).unwrap(),
            Value::Str(if index % 2 == 0 { "1.2345679" } else { "2.5" }.into()),
        );
    }
    assert_eq!(CAST_TYPE_RESOLUTIONS.with(Cell::get), 1);
    assert_eq!(catalog.resolutions.get(), 2);
    assert_eq!(catalog.callbacks.get(), 10_000);
}
