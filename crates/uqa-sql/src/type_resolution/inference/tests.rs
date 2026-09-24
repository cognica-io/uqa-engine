//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{ast::BinaryOp, RowSchema};
use uqa_core::{memory::MemoryBudget, CancellationToken, Value};

fn infer(
    expression: &ScalarExpr,
    schema: &RowSchema,
    params: &[SQLParam],
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<ColumnType>>, SQLError> {
    scalar_type_inner_with_control(expression, schema, params, None, control)
}

fn domain() -> ColumnType {
    ColumnType::Domain {
        schema: "public".into(),
        name: "positive_integer".into(),
        oid: 90001,
        base: Box::new(ColumnType::Integer),
    }
}

#[test]
fn scalar_inference_retains_borrowed_schema_and_parameter_type_payloads() {
    let budget = MemoryBudget::new(65536);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let schema = RowSchema::with_types(vec!["value".into()], vec![Some(domain())]);
    let params = [SQLParam::typed_scalar(Value::Int(7), domain())];
    for expression in [
        ScalarExpr::Column("value".into()),
        ScalarExpr::Position(0),
        ScalarExpr::Param(1),
    ] {
        let output = infer(&expression, &schema, &params, &control)
            .unwrap()
            .unwrap();
        assert_eq!(*output, domain());
        assert!(output.reserved_bytes() > 0);
        assert_eq!(budget.used(), output.reserved_bytes());
        drop(output);
        assert_eq!(budget.used(), 0);
    }
    assert_eq!(schema.column_type(0), Some(&domain()));
}

#[test]
fn scalar_inference_preserves_common_array_case_and_operator_types() {
    let budget = MemoryBudget::new(65536);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let schema = RowSchema::with_types(
        vec!["short".into(), "long".into()],
        vec![
            Some(ColumnType::Varchar(Some(3))),
            Some(ColumnType::Varchar(Some(12))),
        ],
    );
    let expressions = [
        (
            ScalarExpr::Array(vec![
                ScalarExpr::Literal(Value::Str("unknown".into())),
                ScalarExpr::Literal(Value::Null),
            ]),
            ColumnType::Array(Box::new(ColumnType::Text)),
        ),
        (
            ScalarExpr::Case {
                base: None,
                when: vec![(
                    ScalarExpr::Literal(Value::Bool(true)),
                    ScalarExpr::Column("short".into()),
                )],
                else_branch: Some(Box::new(ScalarExpr::Column("long".into()))),
            },
            ColumnType::Varchar(Some(3)),
        ),
        (
            ScalarExpr::Binary {
                op: BinaryOp::Add,
                lhs: Box::new(ScalarExpr::Literal(Value::Int(1))),
                rhs: Box::new(ScalarExpr::Literal(Value::Int(i64::MAX))),
            },
            ColumnType::BigInteger,
        ),
        (
            ScalarExpr::Cast {
                expr: Box::new(ScalarExpr::Literal(Value::Null)),
                ty: "integer[][]".into(),
            },
            ColumnType::Array(Box::new(ColumnType::Array(Box::new(ColumnType::Integer)))),
        ),
    ];
    for (expression, expected) in expressions {
        let output = infer(&expression, &schema, &[], &control).unwrap().unwrap();
        assert_eq!(*output, expected);
        assert_eq!(
            super::super::scalar_type(&expression, &schema, &[]).unwrap(),
            Some(expected)
        );
        assert_eq!(budget.used(), output.reserved_bytes());
        drop(output);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn scalar_inference_releases_partial_type_construction_on_quota_and_both_tokens() {
    let token = CancellationToken::new();
    let schema = RowSchema::with_types(vec!["value".into()], vec![Some(domain())]);
    let expression = ScalarExpr::Array(vec![ScalarExpr::Column("value".into())]);
    let generous = MemoryBudget::new(65536);
    let control = ProductionControl::new(&generous, &token, &token);
    let copied = domain().clone_with_control(&control).unwrap();
    let limit = copied.reserved_bytes();
    drop(copied);
    let budget = MemoryBudget::new(limit);
    let control = ProductionControl::new(&budget, &token, &token);
    assert_eq!(
        infer(&expression, &schema, &[], &control)
            .unwrap_err()
            .sqlstate(),
        Some("53200")
    );
    assert_eq!(budget.used(), 0);
    for cancel_original in [false, true] {
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&generous, &original, &invoking);
        if cancel_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        assert_eq!(
            infer(&expression, &schema, &[], &control)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(generous.used(), 0);
    }
}

#[test]
fn scalar_inference_preserves_cast_and_comparison_error_states() {
    let budget = MemoryBudget::new(65536);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let schema = RowSchema::default();
    for expression in [
        ScalarExpr::Cast {
            expr: Box::new(ScalarExpr::Array(vec![ScalarExpr::Literal(Value::Int(1))])),
            ty: "integer".into(),
        },
        ScalarExpr::Binary {
            op: BinaryOp::Equal,
            lhs: Box::new(ScalarExpr::TypedLiteral {
                value: Value::Str("{}".into()),
                ty: "json".into(),
                bound_type: Some(ColumnType::Json),
                parameter_index: None,
            }),
            rhs: Box::new(ScalarExpr::TypedLiteral {
                value: Value::Str("{}".into()),
                ty: "json".into(),
                bound_type: Some(ColumnType::Json),
                parameter_index: None,
            }),
        },
    ] {
        let ordinary = super::super::scalar_type(&expression, &schema, &[]).unwrap_err();
        let controlled = infer(&expression, &schema, &[], &control).unwrap_err();
        assert_eq!(ordinary.sqlstate(), controlled.sqlstate());
        assert_eq!(ordinary.to_string(), controlled.to_string());
        assert_eq!(budget.used(), 0);
    }
}
