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
fn other_enum_operations_and_plain_expressions_allocate_no_call_state() {
    let prepared = PreparedExpressions::scalars(vec![
        expression(Operation::Equal),
        expression(Operation::NotEqual),
        ScalarExpr::Literal(Value::Int(3)),
    ]);
    assert!(prepared.calls.is_none());
    assert_eq!(prepared.calls().enums.capacity(), 0);
    assert_eq!(evaluate(&prepared, 0, 5, 4).unwrap(), Value::Bool(false));
    assert_eq!(evaluate(&prepared, 1, 11, 8).unwrap(), Value::Bool(true));
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
