//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[derive(Default)]
struct FixedCatalog {
    live: LiveCatalog,
    scope: uqa_sql::expr::CatalogCastScope,
    changed_scope: uqa_sql::expr::CatalogCastScope,
    change_during_operand: Cell<bool>,
}

impl EngineHook for FixedCatalog {
    fn nextval(&self, _: &str) -> Result<i64, SQLError> {
        unreachable!()
    }
    fn currval(&self, _: &str) -> Result<i64, SQLError> {
        unreachable!()
    }
    fn setval(&self, _: &str, _: i64, _: bool) -> Result<i64, SQLError> {
        unreachable!()
    }
    fn catalog_cast_scope(&self) -> Option<&uqa_sql::expr::CatalogCastScope> {
        Some(if self.live.narrow.get() {
            &self.changed_scope
        } else {
            &self.scope
        })
    }
    fn resolve_type_name(&self, name: &str) -> Result<Option<ColumnType>, String> {
        self.live.resolve_type_name(if name == "public.chosen" {
            "chosen"
        } else {
            name
        })
    }
    fn cast_domain(
        &self,
        value: &Value,
        source: Option<&str>,
        target: &ColumnType,
    ) -> Result<Option<Value>, SQLError> {
        self.live.cast_domain(value, source, target)
    }
    fn call_bound_user_function(
        &self,
        binding: &FunctionBinding,
        args: &[(Option<String>, Value)],
    ) -> Option<Result<Value, SQLError>> {
        let result = self.live.call_bound_user_function(binding, args);
        if self.change_during_operand.get() {
            self.live.narrow.set(true);
        }
        result
    }
}

#[test]
fn fixed_catalog_casts_resolve_names_once_and_keep_each_rows_constraints() {
    let prepared = prepare(ScalarExpr::Column("v".into()), "public.chosen");
    let schema = RowSchema::with_types(vec!["v".into()], vec![Some(ColumnType::Integer)]);
    let catalog = FixedCatalog::default();
    let budget = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    CAST_TYPE_RESOLUTIONS.with(|count| count.set(0));
    for value in 0..1_000 {
        assert_eq!(
            *evaluate(&prepared, &schema, Value::Int(value), &catalog, &control).unwrap(),
            Value::Int(value)
        );
    }
    assert_eq!(catalog.live.resolutions.get(), 2);
    assert_eq!(catalog.live.callbacks.get(), 1_000);
    assert_eq!(CAST_TYPE_RESOLUTIONS.with(Cell::get), 1);
    assert!(matches!(
        prepared.calls().row_casts[0].1.get(),
        Some(RowCast::Catalog { .. })
    ));
    assert!(budget.used() > 0 && budget.peak() <= 4096);
    catalog.live.fail.set(true);
    assert_eq!(
        evaluate(&prepared, &schema, Value::Int(1), &catalog, &control)
            .unwrap_err()
            .sqlstate(),
        Some("23514")
    );
    assert_eq!(catalog.live.callbacks.get(), 1_001);
    let observed = (catalog.live.resolutions.get(), catalog.live.callbacks.get());
    token.cancel();
    assert_eq!(
        evaluate(&prepared, &schema, Value::Int(1), &catalog, &control)
            .unwrap_err()
            .sqlstate(),
        Some("57014")
    );
    assert_eq!(
        observed,
        (catalog.live.resolutions.get(), catalog.live.callbacks.get())
    );
    drop(prepared);
    assert_eq!(budget.used(), 0);
}

#[test]
fn fixed_cast_descriptors_do_not_cross_catalogs_or_row_schemas() {
    let prepared = prepare(ScalarExpr::Column("v".into()), "public.chosen");
    let integer = RowSchema::with_types(vec!["v".into()], vec![Some(ColumnType::Integer)]);
    let real = RowSchema::with_types(vec!["v".into()], vec![Some(ColumnType::DoublePrecision)]);
    let catalog = FixedCatalog::default();
    let control = ProductionControl::uncontrolled();
    for (schema, value, expected) in [
        (&integer, Value::Int(32_768), Value::Int(32_768)),
        (&real, Value::Float(2.5), Value::Int(2)),
        (&integer, Value::Int(7), Value::Int(7)),
    ] {
        assert_eq!(
            *evaluate(&prepared, schema, value, &catalog, &control).unwrap(),
            expected
        );
    }
    assert_eq!(catalog.live.resolutions.get(), 4);
    let other = FixedCatalog::default();
    other.live.narrow.set(true);
    assert_eq!(
        evaluate(&prepared, &integer, Value::Int(32_768), &other, &control)
            .unwrap_err()
            .sqlstate(),
        Some("22003")
    );
    assert_eq!(other.live.resolutions.get(), 2);
    assert_eq!(other.live.callbacks.get(), 1);
}

#[test]
fn operand_changes_catalog_before_retained_cast_validation_without_reexecution() {
    let operand = ScalarExpr::Func {
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
                return_type: Some("pg_catalog.int8".into()),
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
    let prepared = prepare(operand, "public.chosen");
    let schema = RowSchema::default();
    let catalog = FixedCatalog::default();
    let control = ProductionControl::uncontrolled();
    for value in [1, 2] {
        catalog.change_during_operand.set(value == 2);
        assert_eq!(
            *evaluate(&prepared, &schema, Value::Null, &catalog, &control).unwrap(),
            Value::Int(value)
        );
    }
    assert_eq!(catalog.live.calls.get(), 2);
    assert_eq!(catalog.live.callbacks.get(), 2);
    assert_eq!(catalog.live.resolutions.get(), 4);
}
