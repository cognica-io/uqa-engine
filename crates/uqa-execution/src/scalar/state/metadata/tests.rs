//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{RowSchema, ScalarExpr, TYPE_RESOLUTIONS};
use crate::scalar::{eval_scalar, state::PreparedExpressions, ScalarEvalContext};
use uqa_core::Value;
use uqa_sql::{ast::BinaryOp, ColumnType, ResultRow, SQLParam};

fn arithmetic(left: ScalarExpr, right: ScalarExpr) -> ScalarExpr {
    ScalarExpr::Binary {
        op: BinaryOp::Add,
        lhs: Box::new(left),
        rhs: Box::new(right),
    }
}

fn schema(ty: ColumnType) -> RowSchema {
    RowSchema::with_types(vec!["v".into()], vec![Some(ty)])
}

#[test]
fn arithmetic_and_negation_infer_once_for_ten_thousand_rows_of_the_same_schema() {
    let column = || ScalarExpr::Column("v".into());
    let plan = PreparedExpressions::scalar(ScalarExpr::UnaryMinus(Box::new(arithmetic(
        column(),
        column(),
    ))));
    let narrow = schema(ColumnType::SmallInteger);
    let wide = schema(ColumnType::Integer);
    TYPE_RESOLUTIONS.set(0);
    for value in 0..10_000 {
        let row = ResultRow::from([("v".into(), Value::Int(value))]);
        let context = ScalarEvalContext::new(Some(&row), &[])
            .with_row_schema(&narrow)
            .with_function_states(plan.calls());
        assert_eq!(
            eval_scalar(&plan, &context).unwrap(),
            Value::Int(-2 * value)
        );
    }
    assert_eq!(TYPE_RESOLUTIONS.get(), 2);
    let row = ResultRow::from([("v".into(), Value::Int(20_000))]);
    let context = ScalarEvalContext::new(Some(&row), &[]).with_function_states(plan.calls());
    assert_eq!(
        eval_scalar(&plan, &context.with_row_schema(&narrow))
            .unwrap_err()
            .sqlstate(),
        Some("22003")
    );
    assert_eq!(
        eval_scalar(&plan, &context.with_row_schema(&wide)).unwrap(),
        Value::Int(-40_000)
    );
    assert_eq!(TYPE_RESOLUTIONS.get(), 4);
}

#[test]
fn real_width_rechecks_changed_schemas_and_parameter_types() {
    let real = schema(ColumnType::Real);
    let double = schema(ColumnType::DoublePrecision);
    let column = || ScalarExpr::Column("v".into());
    let plan = PreparedExpressions::scalar(arithmetic(column(), column()));
    let input = 0.1_f64;
    let row = ResultRow::from([("v".into(), Value::Float(input))]);
    let context = ScalarEvalContext::new(Some(&row), &[]).with_function_states(plan.calls());
    TYPE_RESOLUTIONS.set(0);
    for _ in 0..10_000 {
        assert_eq!(
            eval_scalar(&plan, &context.with_row_schema(&real)).unwrap(),
            Value::Float(f64::from(input as f32 + input as f32))
        );
    }
    assert_eq!(TYPE_RESOLUTIONS.get(), 1);
    assert_eq!(
        eval_scalar(&plan, &context.with_row_schema(&double)).unwrap(),
        Value::Float(input + input)
    );
    assert_eq!(TYPE_RESOLUTIONS.get(), 2);

    let plan = PreparedExpressions::scalar(arithmetic(ScalarExpr::Param(1), ScalarExpr::Param(1)));
    let params = |ty| [SQLParam::typed_scalar(Value::Int(20_000), ty)];
    let narrow = params(ColumnType::SmallInteger);
    let wide = params(ColumnType::Integer);
    for (parameters, overflow) in [(&narrow, true), (&wide, false), (&narrow, true)] {
        let context = ScalarEvalContext::new(None, parameters).with_function_states(plan.calls());
        let value = eval_scalar(&plan, &context);
        if overflow {
            assert_eq!(value.unwrap_err().sqlstate(), Some("22003"));
        } else {
            assert_eq!(value.unwrap(), Value::Int(40_000));
        }
    }
}

#[test]
fn constructors_resolve_once_but_evaluate_every_row_and_follow_the_invoking_schema() {
    let values = PreparedExpressions::scalar(ScalarExpr::Row(vec![
        ScalarExpr::Column("v".into()),
        ScalarExpr::Array(vec![ScalarExpr::Column("v".into())]),
    ]));
    let input = schema(ColumnType::Integer);
    crate::scalar::state::CONSTRUCTOR_OBSERVATIONS.set(0);
    let evaluate = |row: &ResultRow, schema: &RowSchema, expected: u32| {
        let context = ScalarEvalContext::new(Some(row), &[])
            .with_row_schema(schema)
            .with_function_states(values.calls());
        let Value::Row(result) = eval_scalar(&values, &context).unwrap() else {
            panic!("ROW")
        };
        let Value::Array(array) = &result.values()[1] else {
            panic!("ARRAY")
        };
        assert_eq!(result.field_types().unwrap()[0].oid, expected);
        assert_eq!(array.element_type_oid(), Some(expected));
        assert_eq!(array.elements(), &[row["v"].clone()]);
        assert_eq!(result.values()[0], row["v"]);
    };
    for value in 0..10_000 {
        let row = ResultRow::from([("v".into(), Value::Int(value))]);
        evaluate(&row, &input, 23);
    }
    assert_eq!(crate::scalar::state::CONSTRUCTOR_OBSERVATIONS.get(), 2);
    let changed = schema(ColumnType::BigInteger);
    let row = ResultRow::from([("v".into(), Value::Int(20))]);
    evaluate(&row, &changed, 20);
    evaluate(&row, &input, 23);
    assert_eq!(crate::scalar::state::CONSTRUCTOR_OBSERVATIONS.get(), 4);
}

#[derive(Default)]
struct ChangingCatalog {
    old: uqa_sql::expr::CatalogCastScope,
    new: uqa_sql::expr::CatalogCastScope,
    second: std::cell::Cell<bool>,
    switch_on_read: std::cell::Cell<bool>,
    lookups: std::cell::Cell<usize>,
    deny_execute: std::cell::Cell<bool>,
    execute_checks: std::cell::Cell<usize>,
}

impl uqa_sql::expr::EngineHook for ChangingCatalog {
    fn nextval(&self, _: &str) -> Result<i64, uqa_sql::SQLError> {
        unreachable!()
    }
    fn currval(&self, _: &str) -> Result<i64, uqa_sql::SQLError> {
        unreachable!()
    }
    fn setval(&self, _: &str, _: i64, _: bool) -> Result<i64, uqa_sql::SQLError> {
        unreachable!()
    }
    fn require_builtin_execute(
        &self,
        _: &uqa_sql::ast::FunctionBinding,
    ) -> Result<(), uqa_sql::SQLError> {
        self.execute_checks.set(self.execute_checks.get() + 1);
        if self.deny_execute.get() {
            Err(uqa_sql::SQLError::Routine {
                sqlstate: "42501".into(),
                message: "execute denied".into(),
            })
        } else {
            Ok(())
        }
    }
    fn catalog_cast_scope(&self) -> Option<&uqa_sql::expr::CatalogCastScope> {
        Some(if self.second.get() {
            &self.new
        } else {
            &self.old
        })
    }
    fn resolve_type_name(&self, name: &str) -> Result<Option<ColumnType>, String> {
        if !matches!(name, "quantity_type" | "app.quantity_type") {
            return Ok(None);
        }
        self.lookups.set(self.lookups.get() + 1);
        let ty = if self.second.get() {
            ColumnType::BigInteger
        } else {
            ColumnType::Integer
        };
        if self.switch_on_read.replace(false) {
            self.second.set(true);
        }
        Ok(Some(ty))
    }
}

#[test]
fn row_field_metadata_follows_catalog_epochs_unqualified_names_and_lookup_callbacks() {
    for (name, changes_on_first_read, lookups) in [
        ("app.quantity_type", false, 2),
        ("quantity_type", false, 3),
        ("app.quantity_type", true, 2),
    ] {
        let prepared =
            PreparedExpressions::scalar(ScalarExpr::Row(vec![ScalarExpr::TypedLiteral {
                value: Value::Int(1),
                ty: name.into(),
                bound_type: None,
                parameter_index: None,
                collation_oid: None,
                composite_source: None,
            }]));
        let row = ResultRow::default();
        let schema = RowSchema::default();
        let catalog = ChangingCatalog::default();
        catalog.switch_on_read.set(changes_on_first_read);
        let context = ScalarEvalContext::new(Some(&row), &[])
            .with_row_schema(&schema)
            .with_function_hook(&catalog)
            .with_function_states(prepared.calls());
        let oid = || {
            let Value::Row(result) = eval_scalar(&prepared, &context).unwrap() else {
                panic!("ROW")
            };
            result.field_types().unwrap()[0].oid
        };
        assert_eq!(oid(), 23);
        assert_eq!(oid(), if changes_on_first_read { 20 } else { 23 });
        catalog.second.set(true);
        assert_eq!(oid(), 20);
        assert_eq!(
            catalog.lookups.get(),
            lookups,
            "{name}, epoch changed during lookup: {changes_on_first_read}"
        );
    }
}

#[test]
fn bound_builtin_dispatch_is_classified_once_and_authorized_for_every_row() {
    use uqa_core::{
        memory::{MemoryBudget, ProductionControl},
        CancellationToken,
    };
    use uqa_sql::ast::{FunctionBinding, FunctionDispatch};
    let function = |name: &str, dispatch, args| {
        let mut binding = FunctionBinding::dispatched(FunctionDispatch::ToHexInt4);
        binding.name = name.into();
        binding.dispatch = dispatch;
        ScalarExpr::Func {
            name: name.into(),
            binding: Some(binding),
            args,
            distinct: false,
            order_by: Vec::new(),
            order_syntax: Default::default(),
            filter: None,
        }
    };
    super::BUILTIN_CLASSIFICATIONS.set(0);
    let expressions = PreparedExpressions::scalars(vec![
        function(
            "pg_catalog.to_hex",
            Some(FunctionDispatch::ToHexInt4),
            vec![ScalarExpr::Column("v".into())],
        ),
        function("pg_catalog.generate_series", None, vec![]),
        function("pg_catalog.version", None, vec![]),
    ]);
    assert_eq!(super::BUILTIN_CLASSIFICATIONS.get(), 3);
    let schema = schema(ColumnType::Integer);
    let hook = ChangingCatalog::default();
    let budget = MemoryBudget::new(32 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    for number in 0..10_000 {
        let row = ResultRow::from([("v".into(), Value::Int(number))]);
        let context = ScalarEvalContext::new(Some(&row), &[])
            .with_row_schema(&schema)
            .with_function_hook(&hook)
            .with_function_states(expressions.calls());
        let before = hook.execute_checks.get();
        let expected = Value::Str(format!("{number:x}"));
        if number % 2 == 0 {
            assert_eq!(
                *crate::scalar::eval_scalar_with_control(&expressions[0], &context, &control)
                    .unwrap(),
                expected
            );
        } else {
            assert_eq!(eval_scalar(&expressions[0], &context).unwrap(), expected);
        }
        assert!(hook.execute_checks.get() > before);
    }
    let context = ScalarEvalContext::new(None, &[])
        .with_function_hook(&hook)
        .with_function_states(expressions.calls());
    assert!(matches!(
        eval_scalar(&expressions[2], &context).unwrap(),
        Value::Str(_)
    ));
    assert_eq!(
        eval_scalar(&expressions[1], &context)
            .unwrap_err()
            .sqlstate(),
        Some("0A000")
    );
    hook.deny_execute.set(true);
    assert_eq!(
        eval_scalar(&expressions[1], &context)
            .unwrap_err()
            .sqlstate(),
        Some("42501")
    );
    assert_eq!(
        eval_scalar(&expressions[0], &context)
            .unwrap_err()
            .sqlstate(),
        Some("42501")
    );
    assert_eq!(super::BUILTIN_CLASSIFICATIONS.get(), 3);
    assert_eq!(budget.used(), 0);
}
