//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::query::relational::values::test_support::Services;
use uqa_core::{memory::MemoryBudget, CancellationToken};
use uqa_sql::{ast::ColumnType, ScalarExpr};

#[test]
fn catalog_scalar_retires_preparation_and_retains_only_the_selected_output() {
    let input = "catalog value".repeat(512);
    let row = uqa_sql::ResultRow::from([("value".into(), Value::Str(input.clone()))]);
    let schema = RowSchema::with_types(vec!["value".into()], vec![Some(ColumnType::Text)]);
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let scalar = ScalarExpr::Column("value".into())
        .copy_for_rewrite(&control)
        .unwrap();
    let mut optimizations = 0;
    let output = evaluate_catalog_scalar_with_control(
        scalar,
        &row,
        &schema,
        &[],
        &Services,
        &control,
        |scalar| {
            optimizations += 1;
            Ok(scalar)
        },
    )
    .unwrap();
    assert_eq!(*output, Value::Str(input));
    assert_eq!(optimizations, 1);
    assert_eq!(memory.used(), output.reserved_bytes());
    drop(output);
    assert_eq!(memory.used(), 0);
}

#[test]
fn catalog_scalar_keeps_lazy_errors_and_releases_cancelled_preparation() {
    use uqa_sql::ast::BinaryOp;
    let scalar = ScalarExpr::Case {
        base: None,
        when: vec![(
            ScalarExpr::Literal(Value::Bool(true)),
            ScalarExpr::Literal(Value::Int(7)),
        )],
        else_branch: Some(Box::new(ScalarExpr::Binary {
            op: BinaryOp::Divide,
            lhs: Box::new(ScalarExpr::Literal(Value::Int(1))),
            rhs: Box::new(ScalarExpr::Literal(Value::Int(0))),
        })),
    };
    let row = uqa_sql::ResultRow::new();
    let schema = RowSchema::default();
    let memory = MemoryBudget::new(1 << 20);
    for cancel in [None, Some(false), Some(true)] {
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&memory, &original, &invoking);
        let scalar = scalar.copy_for_rewrite(&control).unwrap();
        if let Some(cancel_original) = cancel {
            if cancel_original {
                original.cancel();
            } else {
                invoking.cancel();
            }
        }
        let output = evaluate_catalog_scalar_with_control(
            scalar,
            &row,
            &schema,
            &[],
            &Services,
            &control,
            Ok,
        );
        if cancel.is_some() {
            assert_eq!(output.unwrap_err().sqlstate(), Some("57014"));
        } else {
            assert_eq!(*output.unwrap(), Value::Int(7));
        }
        assert_eq!(memory.used(), 0);
    }
}
