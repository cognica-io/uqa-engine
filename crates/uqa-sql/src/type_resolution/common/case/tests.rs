//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};

fn conditional(condition: ScalarExpr) -> ScalarExpr {
    ScalarExpr::Case {
        base: None,
        when: vec![(condition, ScalarExpr::Column("chosen".into()))],
        else_branch: Some(Box::new(ScalarExpr::Column("otherwise".into()))),
    }
}

fn inferred(
    expression: &ScalarExpr,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<ColumnType>>, SQLError> {
    let ty = match expression {
        ScalarExpr::Column(name) if name == "chosen" => ColumnType::Varchar(Some(3)),
        ScalarExpr::Column(name) if name == "otherwise" => ColumnType::Varchar(Some(9)),
        ScalarExpr::Literal(Value::Int(_)) => ColumnType::Integer,
        _ => ColumnType::Boolean,
    };
    ty.clone_with_control(control).map(Some).map_err(Into::into)
}

#[test]
fn constant_case_refinement_preserves_lazy_branch_modifiers() {
    let budget = MemoryBudget::new(64 * 1024);
    let cancellation = CancellationToken::new();
    let control = ProductionControl::new(&budget, &cancellation, &cancellation);
    for (condition, expected) in [
        (
            ScalarExpr::Literal(Value::Bool(true)),
            ColumnType::Varchar(Some(3)),
        ),
        (
            ScalarExpr::Literal(Value::Bool(false)),
            ColumnType::Varchar(Some(9)),
        ),
        (
            ScalarExpr::Column("condition".into()),
            ColumnType::Varchar(None),
        ),
    ] {
        let output = case_output_type_with_control(
            &conditional(condition),
            &ColumnType::Varchar(None),
            &mut |expression| inferred(expression, &control),
            &control,
        )
        .unwrap();
        assert_eq!(*output, expected);
        assert_eq!(budget.used(), output.reserved_bytes());
        drop(output);
        assert_eq!(budget.used(), 0);
    }
    let mut inferred_else = false;
    let output = case_output_type_with_control(
        &conditional(ScalarExpr::Literal(Value::Bool(true))),
        &ColumnType::Varchar(None),
        &mut |expression| {
            if matches!(expression, ScalarExpr::Column(name) if name == "otherwise") {
                inferred_else = true;
            }
            inferred(expression, &control)
        },
        &control,
    )
    .unwrap();
    assert_eq!(*output, ColumnType::Varchar(Some(3)));
    assert!(!inferred_else);
}

#[test]
fn simple_case_uses_shared_constant_cast_and_binary_comparison_producers() {
    let budget = MemoryBudget::new(64 * 1024);
    let cancellation = CancellationToken::new();
    let control = ProductionControl::new(&budget, &cancellation, &cancellation);
    let mut expression = conditional(ScalarExpr::Literal(Value::Str("1".into())));
    if let ScalarExpr::Case { base, .. } = &mut expression {
        *base = Some(Box::new(ScalarExpr::Literal(Value::Int(1))));
    }
    let output = case_output_type_with_control(
        &expression,
        &ColumnType::Varchar(None),
        &mut |expression| inferred(expression, &control),
        &control,
    )
    .unwrap();
    assert_eq!(*output, ColumnType::Varchar(Some(3)));
    assert_eq!(budget.used(), output.reserved_bytes());
    drop(output);
    assert_eq!(budget.used(), 0);
}

#[test]
fn constant_semantic_errors_preserve_fallback_but_resource_errors_propagate() {
    let budget = MemoryBudget::new(64 * 1024);
    let cancellation = CancellationToken::new();
    let control = ProductionControl::new(&budget, &cancellation, &cancellation);
    let invalid = ScalarExpr::Cast {
        expr: Box::new(ScalarExpr::Literal(Value::Str("bad".into()))),
        ty: "integer".into(),
    };
    let output = case_output_type_with_control(
        &conditional(invalid),
        &ColumnType::Varchar(None),
        &mut |expression| inferred(expression, &control),
        &control,
    )
    .unwrap();
    assert_eq!(*output, ColumnType::Varchar(None));
    drop(output);
    assert_eq!(budget.used(), 0);
    let budget = MemoryBudget::new(32);
    let control = ProductionControl::new(&budget, &cancellation, &cancellation);
    let value = ScalarExpr::Literal(Value::Str("x".repeat(1024)));
    assert_eq!(
        case_output_type_with_control(
            &conditional(value),
            &ColumnType::Varchar(None),
            &mut |expression| inferred(expression, &control),
            &control
        )
        .unwrap_err()
        .sqlstate(),
        Some("53200")
    );
    assert_eq!(budget.used(), 0);
}

#[test]
fn case_refinement_honors_both_cancellation_sources() {
    let budget = MemoryBudget::new(4096);
    for cancel_original in [false, true] {
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if cancel_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        let control = ProductionControl::new(&budget, &original, &invoking);
        assert_eq!(
            case_output_type_with_control(
                &conditional(ScalarExpr::Literal(Value::Bool(true))),
                &ColumnType::Varchar(None),
                &mut |expression| inferred(expression, &control),
                &control
            )
            .unwrap_err()
            .sqlstate(),
            Some("57014")
        );
        assert_eq!(budget.used(), 0);
    }
}
