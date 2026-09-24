//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::RowSchema;
use uqa_core::{memory::MemoryBudget, CancellationToken};

fn domain(base: ColumnType) -> ColumnType {
    ColumnType::Domain {
        schema: "app".into(),
        name: "bounded".into(),
        oid: 91_001,
        base: Box::new(base),
    }
}

#[test]
fn legacy_vector_cast_sources_retain_domain_identity_without_changing_operators() {
    let budget = MemoryBudget::new(65_536);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let ty = domain(ColumnType::Int2Vector);
    let schema = RowSchema::with_types(vec!["value".into()], vec![Some(ty.clone())]);
    let params = [SQLParam::typed_scalar(Value::Null, ty.clone())];
    for expression in [
        ScalarExpr::Column("value".into()),
        ScalarExpr::Position(0),
        ScalarExpr::Param(1),
        ScalarExpr::TypedLiteral {
            value: Value::Null,
            ty: "ignored_spelling".into(),
            bound_type: Some(ty.clone()),
            parameter_index: None,
        },
    ] {
        let name =
            scalar_cast_source_type_name_with_control(&expression, &schema, &params, &control)
                .unwrap()
                .unwrap();
        assert_eq!(&*name, &ty.sql_name());
        drop(name);
        let name = scalar_operand_type_name_with_control(&expression, &schema, &params, &control)
            .unwrap()
            .unwrap();
        assert_eq!(&*name, "int2vector");
        drop(name);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn operand_names_keep_bound_type_and_unknown_literal_precedence() {
    let budget = MemoryBudget::new(65_536);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let ty = domain(domain(ColumnType::Varchar(Some(12))));
    let schema = RowSchema::with_types(vec!["value".into()], vec![Some(ty.clone())]);
    for (expression, expected) in [
        (ScalarExpr::Column("value".into()), "character varying(12)"),
        (
            ScalarExpr::TypedLiteral {
                value: Value::Int(1),
                ty: "ignored_spelling".into(),
                bound_type: Some(domain(ColumnType::SmallInteger)),
                parameter_index: None,
            },
            "smallint",
        ),
        (
            ScalarExpr::Cast {
                expr: Box::new(ScalarExpr::Literal(Value::Int(1))),
                ty: "pg_catalog.int8".into(),
            },
            "pg_catalog.int8",
        ),
        (ScalarExpr::Literal(Value::Int(i64::MAX)), "bigint"),
    ] {
        let name = scalar_operand_type_name_with_control(&expression, &schema, &[], &control)
            .unwrap()
            .unwrap();
        assert_eq!(&*name, expected);
        assert_eq!(budget.used(), name.reserved_bytes());
        drop(name);
        assert_eq!(budget.used(), 0);
    }
    for expression in [
        ScalarExpr::Literal(Value::Str("unknown".into())),
        ScalarExpr::Column("missing".into()),
    ] {
        assert!(
            scalar_operand_type_name_with_control(&expression, &schema, &[], &control)
                .unwrap()
                .is_none()
        );
        assert_eq!(budget.used(), 0);
    }
    assert_eq!(schema.column_type(0), Some(&ty));
}

#[test]
fn integer_operand_widths_keep_literal_domain_and_parameter_rules() {
    let budget = MemoryBudget::new(65_536);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let schema = RowSchema::with_types(
        vec!["narrow".into()],
        vec![Some(domain(ColumnType::SmallInteger))],
    );
    let narrow = ScalarExpr::Column("narrow".into());
    let params = [SQLParam::typed_scalar(
        Value::Int(1),
        domain(ColumnType::BigInteger),
    )];
    for (right, expected) in [
        (narrow.clone(), Some(IntegerWidth::SmallInt)),
        (
            ScalarExpr::Literal(Value::Int(1)),
            Some(IntegerWidth::Integer),
        ),
        (
            ScalarExpr::Literal(Value::Int(i64::MAX)),
            Some(IntegerWidth::BigInt),
        ),
        (ScalarExpr::Param(1), Some(IntegerWidth::BigInt)),
        (
            ScalarExpr::UnaryMinus(Box::new(ScalarExpr::Binary {
                op: BinaryOp::Add,
                lhs: Box::new(ScalarExpr::Literal(Value::Int(1))),
                rhs: Box::new(ScalarExpr::Literal(Value::Int(i64::MAX))),
            })),
            Some(IntegerWidth::BigInt),
        ),
        (ScalarExpr::Literal(Value::Str("unknown".into())), None),
    ] {
        assert_eq!(
            scalar_integer_operation_width_with_control(
                &narrow, &right, &schema, &params, &control
            )
            .unwrap(),
            expected
        );
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn optional_operand_inference_does_not_swallow_allocation_failure() {
    let budget = MemoryBudget::new(1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let held = control.copy_text("previous result").unwrap();
    let retained = budget.used();
    let ty = ColumnType::Domain {
        schema: "app".into(),
        name: "n".repeat(4096),
        oid: 91_002,
        base: Box::new(ColumnType::Integer),
    };
    let schema = RowSchema::with_types(vec!["value".into()], vec![Some(ty)]);
    let expression = ScalarExpr::Column("value".into());
    assert_eq!(
        scalar_operand_type_name_with_control(&expression, &schema, &[], &control)
            .unwrap_err()
            .sqlstate(),
        Some("53200")
    );
    assert_eq!(
        scalar_integer_operation_width_with_control(
            &expression,
            &expression,
            &schema,
            &[],
            &control
        )
        .unwrap_err()
        .sqlstate(),
        Some("53200")
    );
    assert_eq!(budget.used(), retained);
    assert_eq!(&*held, "previous result");
    drop(held);
    assert_eq!(budget.used(), 0);
}

#[test]
fn optional_operand_paths_check_both_cancellation_scopes() {
    let budget = MemoryBudget::new(1024);
    let schema = RowSchema::default();
    for cancel_original in [true, false] {
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        if cancel_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        let expression = ScalarExpr::Literal(Value::Str("unknown".into()));
        assert_eq!(
            scalar_operand_type_name_with_control(&expression, &schema, &[], &control)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(
            scalar_integer_operation_width_with_control(
                &expression,
                &expression,
                &schema,
                &[],
                &control
            )
            .unwrap_err()
            .sqlstate(),
            Some("57014")
        );
        assert_eq!(budget.used(), 0);
    }
}
