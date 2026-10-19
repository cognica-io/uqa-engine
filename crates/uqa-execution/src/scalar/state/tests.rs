//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{eval_scalar, ScalarEvalContext};
use std::sync::Arc;
use uqa_core::{DatumValue, Value};
use uqa_sql::{
    ast::{EnumFunctionOperation as Operation, FunctionBinding, FunctionOrderSyntax},
    expr::{enums::EnumLabelCatalog, enums::EnumTypeLabels, EngineHook},
    ResultRow, SQLError,
};

#[test]
fn analytical_numeric_predicates_do_not_probe_enum_call_state() {
    let uqa_sql::Statement::Select(mut query) = uqa_sql::compile(
        "SELECT ship_day BETWEEN 365 AND 2190 AND discount BETWEEN 2 AND 8 AND quantity < 40",
    )
    .unwrap()
    .remove(0) else {
        panic!("SELECT predicate");
    };
    let prepared = PreparedExpressions::scalar(
        uqa_sql::plan::ExpressionPlan::lower(query.projections.remove(0).expr).scalar,
    );
    assert_eq!(prepared.calls().enums.len(), 5);
    COMPARISON_LOOKUPS.with(|count| count.set(0));
    for id in 0..20_000 {
        let sample = id ^ 0x0135_27c2;
        let (ship_day, discount, quantity) = (sample % 2_500, sample % 11, 1 + sample % 50);
        let row = ResultRow::from([
            ("ship_day".into(), Value::Int(ship_day)),
            ("discount".into(), Value::Int(discount)),
            ("quantity".into(), Value::Int(quantity)),
        ]);
        let context = ScalarEvalContext::new(Some(&row), &[])
            .with_function_hook(&Catalog)
            .with_function_states(prepared.calls());
        assert_eq!(
            eval_scalar(&prepared, &context).unwrap(),
            Value::Bool(
                (365..=2190).contains(&ship_day) && (2..=8).contains(&discount) && quantity < 40
            ),
            "row {id}"
        );
    }
    assert_eq!(COMPARISON_LOOKUPS.with(std::cell::Cell::get), 0);
}

struct Catalog;

impl EnumLabelCatalog for Catalog {
    fn enum_type_labels(&self, oid: u32) -> Result<Option<Arc<EnumTypeLabels>>, SQLError> {
        Ok(matches!(oid, 16_384 | 16_400).then(|| {
            Arc::new(EnumTypeLabels {
                type_oid: oid,
                labels: Vec::new(),
            })
        }))
    }

    fn enum_label_position(&self, oid: u32) -> Result<Option<(u32, usize)>, SQLError> {
        Ok(match oid {
            2 => Some((16_384, 0)),
            5 => Some((16_384, 1)),
            4 => Some((16_384, 2)),
            11 => Some((16_400, 0)),
            8 => Some((16_400, 1)),
            _ => None,
        })
    }

    fn enum_label_uncommitted(&self, _: u32) -> bool {
        false
    }

    fn enum_type_name(&self, oid: u32) -> Result<Option<String>, SQLError> {
        Ok(Some(
            if oid == 16_384 {
                "first_enum"
            } else {
                "second_enum"
            }
            .into(),
        ))
    }

    fn has_enum_types(&self) -> bool {
        true
    }
}

impl EngineHook for Catalog {
    fn nextval(&self, _: &str) -> Result<i64, SQLError> {
        unreachable!()
    }
    fn currval(&self, _: &str) -> Result<i64, SQLError> {
        unreachable!()
    }
    fn setval(&self, _: &str, _: i64, _: bool) -> Result<i64, SQLError> {
        unreachable!()
    }
    fn enum_labels(&self) -> Option<&dyn EnumLabelCatalog> {
        Some(self)
    }
}

fn expression(operation: Operation) -> ScalarExpr {
    ScalarExpr::Func {
        name: operation.label().into(),
        binding: Some(FunctionBinding::dispatched(FunctionDispatch::Enum {
            operation,
            type_oid: 16_400,
        })),
        args: vec![
            ScalarExpr::Column("left".into()),
            ScalarExpr::Column("right".into()),
        ],
        distinct: false,
        order_by: Vec::new(),
        order_syntax: FunctionOrderSyntax::default(),
        filter: None,
    }
}

fn evaluate(
    prepared: &PreparedExpressions<Vec<ScalarExpr>>,
    index: usize,
    left: u32,
    right: u32,
) -> Result<Value, SQLError> {
    let physical = |oid: u32| Value::Datum(DatumValue::new(16_400, 0, oid.to_le_bytes().to_vec()));
    let row = ResultRow::from([
        ("left".into(), physical(left)),
        ("right".into(), physical(right)),
    ]);
    let context = ScalarEvalContext::new(Some(&row), &[])
        .with_function_hook(&Catalog)
        .with_function_states(prepared.calls());
    eval_scalar(&prepared[index], &context)
}

#[test]
fn controlled_function_calls_retain_catalog_and_prepared_enum_state() {
    use uqa_core::{
        memory::{MemoryBudget, ProductionControl},
        CancellationToken,
    };

    let mut greatest = expression(Operation::Compare);
    if let ScalarExpr::Func { name, binding, .. } = &mut greatest {
        *name = "greatest".into();
        *binding = None;
    }
    let mut any = expression(Operation::Compare);
    if let ScalarExpr::Func {
        name,
        binding,
        args,
        ..
    } = &mut any
    {
        *name = "any_operator".into();
        *binding = Some(FunctionBinding::dispatched(FunctionDispatch::AnyOperator));
        args[1] = ScalarExpr::Array(vec![args[1].clone()]);
        args.push(ScalarExpr::Literal(Value::Str("<".into())));
    }
    let physical = |oid: u32| Value::Datum(DatumValue::new(16_400, 0, oid.to_le_bytes().to_vec()));
    for (expression, expected) in [
        (expression(Operation::Compare), Value::Int(-1)),
        (expression(Operation::Smaller), physical(5)),
        (greatest, physical(4)),
        (any, Value::Bool(true)),
    ] {
        let prepared = PreparedExpressions::scalar(expression);
        let budget = MemoryBudget::new(16_384);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        for (left, right) in [(5, 4), (11, 8)] {
            let row = ResultRow::from([
                ("left".into(), physical(left)),
                ("right".into(), physical(right)),
            ]);
            let context = ScalarEvalContext::new(Some(&row), &[])
                .with_function_hook(&Catalog)
                .with_function_states(prepared.calls());
            let result = crate::scalar::evaluator::eval_scalar_inner(&prepared, &context, &control);
            if left == 5 {
                let result = result.unwrap();
                assert!(result.has_same_representation(&expected));
                assert_eq!(budget.used(), result.reserved_bytes());
            } else {
                let error = result.unwrap_err();
                assert_eq!(error.sqlstate(), Some("XX000"));
                assert!(error.to_string().contains("first_enum"));
            }
            assert_eq!(budget.used(), 0);
        }
    }
}

#[test]
fn calls_keep_separate_state_across_rows_and_clones_start_fresh() {
    let prepared = PreparedExpressions::scalars(vec![
        expression(Operation::Compare),
        expression(Operation::Compare),
    ]);
    assert_eq!(prepared.calls().enums.len(), 2);
    assert_eq!(evaluate(&prepared, 0, 5, 4).unwrap(), Value::Int(-1));
    // Moving the owner cannot change its call identities.
    let prepared = Box::new(prepared);
    assert_eq!(evaluate(&prepared, 1, 11, 8).unwrap(), Value::Int(-1));
    let error = evaluate(&prepared, 0, 11, 8).unwrap_err();
    assert_eq!(error.sqlstate(), Some("XX000"));
    assert!(error
        .to_string()
        .contains("enum value 11 not found in cache for enum first_enum"));
    let cloned = prepared.as_ref().clone();
    assert_eq!(evaluate(&cloned, 0, 11, 8).unwrap(), Value::Int(-1));
    assert_eq!(evaluate(&cloned, 1, 5, 4).unwrap(), Value::Int(-1));
    assert_eq!(prepared.calls().enums.len(), 2);
}

#[test]
fn other_enum_operations_and_plain_expressions_allocate_no_comparison_state() {
    let prepared = PreparedExpressions::scalars(vec![
        expression(Operation::Equal),
        expression(Operation::NotEqual),
        ScalarExpr::Literal(Value::Int(3)),
    ]);
    // Equality needs no comparison state; only the four column references retain a slot each.
    assert_eq!(prepared.calls().enums.capacity(), 0);
    assert!(prepared.calls().constant_casts.is_empty());
    assert_eq!(prepared.calls().columns.len(), 4);
    assert_eq!(evaluate(&prepared, 0, 5, 4).unwrap(), Value::Bool(false));
    assert_eq!(evaluate(&prepared, 1, 11, 8).unwrap(), Value::Bool(true));
    let literals = PreparedExpressions::scalars(vec![ScalarExpr::Literal(Value::Int(3))]);
    assert!(literals.calls.is_none());
}

#[test]
fn binary_operators_keep_independent_state_after_owner_moves() {
    let binary = || ScalarExpr::Binary {
        op: BinaryOp::Less,
        lhs: Box::new(ScalarExpr::Column("left".into())),
        rhs: Box::new(ScalarExpr::Column("right".into())),
    };
    let prepared = PreparedExpressions::scalars(vec![binary(), binary()]);
    assert_eq!(evaluate(&prepared, 0, 5, 4).unwrap(), Value::Bool(true));
    let prepared = Box::new(prepared);
    assert_eq!(evaluate(&prepared, 1, 11, 8).unwrap(), Value::Bool(true));
    assert_eq!(
        evaluate(&prepared, 0, 11, 8).unwrap_err().sqlstate(),
        Some("XX000")
    );
    assert_eq!(
        evaluate(&prepared.clone(), 0, 11, 8).unwrap(),
        Value::Bool(true)
    );
    assert_eq!(prepared.calls().enums.len(), 2);
}

#[test]
fn between_bounds_retain_separate_enum_state_after_primitive_rows() {
    let prepared = PreparedExpressions::scalar(ScalarExpr::Between {
        expr: Box::new(ScalarExpr::Column("value".into())),
        low: Box::new(ScalarExpr::Column("low".into())),
        high: Box::new(ScalarExpr::Column("high".into())),
    });
    let evaluate = |prepared: &PreparedExpressions<ScalarExpr>, values: [Value; 3]| {
        let row = ["value", "low", "high"]
            .into_iter()
            .map(str::to_owned)
            .zip(values)
            .collect::<ResultRow>();
        let context = ScalarEvalContext::new(Some(&row), &[])
            .with_function_hook(&Catalog)
            .with_function_states(prepared.calls());
        eval_scalar(prepared, &context)
    };
    let physical = |oid: u32| Value::Datum(DatumValue::new(16_400, 0, oid.to_le_bytes().to_vec()));
    COMPARISON_LOOKUPS.with(|count| count.set(0));
    assert_eq!(
        evaluate(&prepared, [Value::Int(1), Value::Int(0), Value::Int(2)]).unwrap(),
        Value::Bool(true)
    );
    assert_eq!(COMPARISON_LOOKUPS.with(std::cell::Cell::get), 0);
    // The even-OID lower comparison does not initialize its private state.
    // The odd-OID upper comparison retains first_enum for later rows.
    assert_eq!(
        evaluate(&prepared, [physical(4), physical(2), physical(5)]).unwrap(),
        Value::Bool(false)
    );
    assert_eq!(COMPARISON_LOOKUPS.with(std::cell::Cell::get), 2);
    let prepared = Box::new(prepared);
    let error = evaluate(&prepared, [physical(8), physical(2), physical(11)]).unwrap_err();
    assert_eq!(error.sqlstate(), Some("XX000"));
    assert!(error.to_string().contains("first_enum"));
    assert_eq!(
        evaluate(&prepared.clone(), [physical(8), physical(2), physical(11)]).unwrap(),
        Value::Bool(false)
    );
}

fn literal_cast(literal: Value, ty: &str) -> ScalarExpr {
    ScalarExpr::Cast {
        implicit: true,
        expr: Box::new(ScalarExpr::Literal(literal)),
        ty: ty.into(),
    }
}

fn evaluate_rows(
    prepared: &PreparedExpressions<ScalarExpr>,
    rows: i64,
) -> Vec<Result<Value, SQLError>> {
    (0..rows)
        .map(|quantity| {
            let row = ResultRow::from([("quantity".into(), Value::Int(quantity))]);
            let context =
                ScalarEvalContext::new(Some(&row), &[]).with_function_states(prepared.calls());
            eval_scalar(prepared, &context)
        })
        .collect()
}

#[test]
fn immutable_literal_casts_convert_once_per_prepared_execution() {
    let prepared = PreparedExpressions::scalar(ScalarExpr::Binary {
        op: uqa_sql::ast::BinaryOp::GreaterEqual,
        lhs: Box::new(ScalarExpr::Column("quantity".into())),
        rhs: Box::new(literal_cast(Value::Int(10), "pg_catalog.\"numeric\"")),
    });
    assert_eq!(prepared.calls().constant_casts.len(), 1);
    CONSTANT_CAST_CONVERSIONS.with(|count| count.set(0));
    let results = evaluate_rows(&prepared, 1_000);
    for (quantity, result) in results.into_iter().enumerate() {
        assert_eq!(
            result.unwrap(),
            Value::Bool(quantity >= 10),
            "row {quantity}"
        );
    }
    assert_eq!(CONSTANT_CAST_CONVERSIONS.with(std::cell::Cell::get), 1);

    // A clone prepares independent state, which converts its literal again once.
    let cloned = prepared.clone();
    CONSTANT_CAST_CONVERSIONS.with(|count| count.set(0));
    assert!(evaluate_rows(&cloned, 100)
        .into_iter()
        .all(|result| result.is_ok()));
    assert_eq!(CONSTANT_CAST_CONVERSIONS.with(std::cell::Cell::get), 1);
}

#[test]
fn unqualified_column_references_retain_their_slot_per_schema() {
    use crate::{OwnedPhysicalRow, PhysicalRow, RowSchema};
    use uqa_sql::ast::ColumnType;
    let schema = |qualifier: &str, columns: [&str; 2]| {
        RowSchema::with_qualified_types(
            qualifier,
            columns.iter().map(|column| (*column).into()).collect(),
            vec![Some(ColumnType::BigInteger), Some(ColumnType::BigInteger)],
        )
    };
    let sum = || {
        PreparedExpressions::scalar(ScalarExpr::Binary {
            op: uqa_sql::ast::BinaryOp::Add,
            lhs: Box::new(ScalarExpr::Column("quantity".into())),
            rhs: Box::new(ScalarExpr::Column("price".into())),
        })
    };
    let evaluate =
        |prepared: &PreparedExpressions<ScalarExpr>, schema: &RowSchema, values: [i64; 2]| {
            let row = OwnedPhysicalRow::new(
                schema.clone(),
                PhysicalRow::from_values(values.iter().copied().map(Value::Int).collect()),
            );
            let context = ScalarEvalContext::from_row_lookup(&row, &[])
                .with_function_states(prepared.calls())
                .with_row_schema(schema);
            eval_scalar(prepared, &context).unwrap()
        };
    let prepared = sum();
    assert_eq!(prepared.calls().columns.len(), 2);
    let lineitem = schema("lineitem", ["quantity", "price"]);
    COLUMN_NAME_LOOKUPS.with(|count| count.set(0));
    for quantity in 0..1_000 {
        assert_eq!(
            evaluate(&prepared, &lineitem, [quantity, 1]),
            Value::Int(quantity + 1)
        );
    }
    assert_eq!(COLUMN_NAME_LOOKUPS.with(std::cell::Cell::get), 2);
    // Rows of another schema, whose columns sit at other positions, resolve by name and leave the retained slots to the first schema.
    let orders = schema("orders", ["price", "quantity"]);
    assert_eq!(evaluate(&prepared, &orders, [1, 5]), Value::Int(6));
    assert_eq!(COLUMN_NAME_LOOKUPS.with(std::cell::Cell::get), 4);
    assert_eq!(evaluate(&prepared, &lineitem, [7, 1]), Value::Int(8));
    assert_eq!(COLUMN_NAME_LOOKUPS.with(std::cell::Cell::get), 4);
    // A clone prepares independent state and resolves each name again once.
    let cloned = prepared.clone();
    for quantity in 0..10 {
        assert_eq!(
            evaluate(&cloned, &lineitem, [quantity, 2]),
            Value::Int(quantity + 2)
        );
    }
    assert_eq!(COLUMN_NAME_LOOKUPS.with(std::cell::Cell::get), 6);
}

#[test]
fn projected_rows_and_qualified_references_retain_their_keys() {
    use crate::{ProjectedRow, ProjectedValueSlot, RowSchema};
    use uqa_sql::ast::ColumnType;
    let schema = RowSchema::with_qualified_types(
        "lineitem",
        vec!["quantity".into(), "price".into()],
        vec![Some(ColumnType::BigInteger), Some(ColumnType::BigInteger)],
    );
    // The projected fields arrive in the opposite order of the schema's columns.
    let fields = ["price".to_string(), "quantity".to_string()];
    let slots = ProjectedValueSlot::compile(schema.columns(), &fields, &[]);
    let prepared = PreparedExpressions::scalar(ScalarExpr::Binary {
        op: uqa_sql::ast::BinaryOp::Add,
        lhs: Box::new(ScalarExpr::Column("quantity".into())),
        rhs: Box::new(ScalarExpr::QualifiedColumn {
            qualifier: "lineitem".into(),
            column: "price".into(),
        }),
    });
    assert_eq!(prepared.calls().columns.len(), 2);
    COLUMN_NAME_LOOKUPS.with(|count| count.set(0));
    for quantity in 0..100 {
        let (price, quantity_value) = (Value::Int(3), Value::Int(quantity));
        let values = [&price, &quantity_value];
        let row = ProjectedRow::new(&schema, &slots, &values, &[]);
        let context =
            ScalarEvalContext::from_row_lookup(&row, &[]).with_function_states(prepared.calls());
        assert_eq!(
            eval_scalar(&prepared, &context).unwrap(),
            Value::Int(quantity + 3)
        );
    }
    assert_eq!(COLUMN_NAME_LOOKUPS.with(std::cell::Cell::get), 2);
}

#[test]
fn search_path_or_failing_literal_casts_convert_at_every_evaluation() {
    // The grammar spells keyword types such as NUMERIC in pg_catalog, while an identifier such as int8 can select another type through search_path.
    for (target, conversions) in [("numeric", 1), ("int8", 50)] {
        let prepared = PreparedExpressions::scalar(ScalarExpr::Binary {
            op: uqa_sql::ast::BinaryOp::GreaterEqual,
            lhs: Box::new(ScalarExpr::Column("quantity".into())),
            rhs: Box::new(literal_cast(Value::Int(10), target)),
        });
        CONSTANT_CAST_CONVERSIONS.with(|count| count.set(0));
        let results = evaluate_rows(&prepared, 50);
        for (quantity, result) in results.into_iter().enumerate() {
            assert_eq!(
                result.unwrap(),
                Value::Bool(quantity >= 10),
                "{target} row {quantity}"
            );
        }
        assert_eq!(
            CONSTANT_CAST_CONVERSIONS.with(std::cell::Cell::get),
            conversions,
            "{target}"
        );
    }

    // A failed conversion is reported by every evaluation instead of being retained.
    let failing = PreparedExpressions::scalar(literal_cast(
        Value::Str("not a number".into()),
        "pg_catalog.\"numeric\"",
    ));
    CONSTANT_CAST_CONVERSIONS.with(|count| count.set(0));
    let results = evaluate_rows(&failing, 3);
    assert!(results.iter().all(|result| result
        .as_ref()
        .is_err_and(|error| error.sqlstate() == Some("22P02"))));
    assert_eq!(CONSTANT_CAST_CONVERSIONS.with(std::cell::Cell::get), 3);
}
