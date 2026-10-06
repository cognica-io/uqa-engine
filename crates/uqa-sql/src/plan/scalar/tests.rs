//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::{
    FunctionBinding, FunctionOrderSyntax, FunctionResolutionError, OperatorResolutionError,
    RoutineInvocationBinding, RoutineVariadicMode, Statement,
};
use crate::plan::ExpressionPlan;
use uqa_core::{memory::MemoryError, Value};

fn expression(sql: &str) -> Expr {
    let Statement::Select(mut query) = crate::compile(sql).unwrap().remove(0) else {
        panic!("expected SELECT expression");
    };
    query.projections.remove(0).expr
}

#[test]
fn controlled_column_lowering_preserves_existing_scalar_shapes_and_input() {
    let budget = MemoryBudget::new(1 << 20);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    for sql in [
        "SELECT a",
        "SELECT t.a",
        "SELECT 'literal'",
        "SELECT $1",
        "SELECT concat(a, '한🙂')",
        "SELECT ARRAY[1, 2, 3]",
        "SELECT ROW(a, 'text')",
        "SELECT a + 1",
        "SELECT -a",
        "SELECT NOT a",
        "SELECT a AND b AND c",
        "SELECT a OR b OR c",
        "SELECT a IS NOT NULL",
        "SELECT a BETWEEN 1 AND 2",
        "SELECT a NOT IN (1, 2, 3)",
        "SELECT CASE a WHEN 1 THEN 'first' ELSE 'last' END",
        "SELECT CASE WHEN a THEN 'yes' END",
        "SELECT CAST(a AS text)",
        "SELECT sum(a ORDER BY b DESC NULLS FIRST) FILTER (WHERE c)",
        "SELECT percentile_cont(0.5) WITHIN GROUP (ORDER BY a DESC NULLS LAST) FILTER (WHERE b)",
        "SELECT sum(a) OVER (PARTITION BY b ORDER BY c ROWS BETWEEN 2 PRECEDING AND 1 FOLLOWING)",
        "SELECT sum(a) OVER (ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW)",
        "SELECT sum(a) OVER (ROWS BETWEEN CURRENT ROW AND UNBOUNDED FOLLOWING)",
    ] {
        let source = expression(sql);
        let before = source.clone();
        let expected = ExpressionPlan::lower(source.clone());
        let lowered =
            ExpressionPlan::lower_column_budgeted(&source, &budget, &original, &invoking).unwrap();
        assert_eq!(*lowered, expected.scalar, "{sql}");
        assert_eq!(source, before, "{sql}");
        assert_eq!(budget.used(), lowered.reserved_bytes());
        drop(lowered);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn function_order_syntax_preserves_direct_arguments_through_lowering() {
    for (sql, expected, direct_count, order_count) in [
        ("SELECT f(a)", FunctionOrderSyntax::Ordinary, 1, 0),
        (
            "SELECT f(a ORDER BY b DESC NULLS FIRST)",
            FunctionOrderSyntax::Ordinary,
            1,
            1,
        ),
        (
            "SELECT f(a) WITHIN GROUP (ORDER BY b DESC NULLS FIRST)",
            FunctionOrderSyntax::WithinGroup,
            1,
            1,
        ),
        (
            "SELECT mode() WITHIN GROUP (ORDER BY b)",
            FunctionOrderSyntax::WithinGroup,
            0,
            1,
        ),
    ] {
        let source = expression(sql);
        let Expr::Func {
            args,
            order_by,
            order_syntax,
            ..
        } = &source
        else {
            panic!("function expression expected");
        };
        assert_eq!(*order_syntax, expected, "{sql}");
        assert_eq!(args.len(), direct_count, "{sql}");
        assert_eq!(order_by.len(), order_count, "{sql}");
        let ScalarExpr::Func {
            args,
            order_by,
            order_syntax,
            ..
        } = ExpressionPlan::lower(source).scalar
        else {
            panic!("function scalar expected");
        };
        assert_eq!(order_syntax, expected, "{sql}");
        assert_eq!(args.len(), direct_count, "{sql}");
        assert_eq!(order_by.len(), order_count, "{sql}");
    }
}

#[test]
fn function_order_syntax_round_trips_without_reclassifying_legacy_json() {
    for syntax in [
        FunctionOrderSyntax::Legacy,
        FunctionOrderSyntax::Ordinary,
        FunctionOrderSyntax::WithinGroup,
    ] {
        let mut source = expression("SELECT f(a) WITHIN GROUP (ORDER BY b)");
        let Expr::Func { order_syntax, .. } = &mut source else {
            unreachable!();
        };
        *order_syntax = syntax;
        let encoded = serde_json::to_value(&source).unwrap();
        assert_eq!(
            encoded["Func"].get("order_syntax").is_none(),
            syntax.is_legacy()
        );
        assert_eq!(serde_json::from_value::<Expr>(encoded).unwrap(), source);
        let scalar = ExpressionPlan::lower(source).scalar;
        let encoded = serde_json::to_value(&scalar).unwrap();
        assert_eq!(
            encoded["Func"].get("order_syntax").is_none(),
            syntax.is_legacy()
        );
        assert_eq!(
            serde_json::from_value::<ScalarExpr>(encoded).unwrap(),
            scalar
        );
    }
}

#[test]
fn copied_bindings_keep_invocation_identity_and_both_error_variants() {
    let budget = MemoryBudget::new(1 << 20);
    let cancellation = CancellationToken::new();
    for error in [
        FunctionResolutionError::UndefinedFunction {
            signature: "f(text)".into(),
        },
        FunctionResolutionError::Operator(Box::new(OperatorResolutionError {
            sqlstate: "42883".into(),
            message: "operator missing".into(),
        })),
    ] {
        let source = Expr::Func {
            order_syntax: crate::ast::FunctionOrderSyntax::Ordinary,
            name: "f".into(),
            binding: Some(FunctionBinding {
                object_id: Some([7; 16]),
                name: "app.f".into(),
                argument_types: vec!["text".into()],
                builtin: false,
                dispatch: None,
                invocation: Some(Box::new(RoutineInvocationBinding {
                    argument_positions: vec![1, 0],
                    argument_targets: vec!["text".into(), "integer".into()],
                    argument_sources: vec![Some("varchar".into()), None],
                    parameter_types: vec!["integer".into(), "text".into()],
                    return_type: Some("text".into()),
                    variadic_mode: RoutineVariadicMode::Expanded { parameter_index: 1 },
                })),
                resolution_error: Some(error),
            }),
            args: vec![Expr::TypedLiteral {
                value: Value::Str("value".into()),
                ty: "text".into(),
            }],
            distinct: false,
            order_by: Vec::new(),
            filter: None,
        };
        let lowered =
            ExpressionPlan::lower_column_budgeted(&source, &budget, &cancellation, &cancellation)
                .unwrap();
        assert_eq!(*lowered, ExpressionPlan::lower(source).scalar);
        assert_eq!(budget.used(), lowered.reserved_bytes());
        drop(lowered);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn borrowed_lowering_copies_only_destination_capacity_and_owned_lowering_moves_payloads() {
    let mut text = String::with_capacity(1 << 18);
    text.push_str("small");
    let address = text.as_ptr();
    let source = Expr::Literal(Value::Str(text));
    let budget = MemoryBudget::new(16);
    let cancellation = CancellationToken::new();
    let lowered =
        ExpressionPlan::lower_column_budgeted(&source, &budget, &cancellation, &cancellation)
            .unwrap();
    let ScalarExpr::Literal(Value::Str(value)) = &*lowered else {
        panic!("string literal");
    };
    assert_eq!(value, "small");
    assert_ne!(value.as_ptr(), address);
    assert_eq!(lowered.reserved_bytes(), value.capacity());
    let owned = ExpressionPlan::lower(source);
    let ScalarExpr::Literal(Value::Str(value)) = owned.scalar else {
        panic!("string literal");
    };
    assert_eq!(value.as_ptr(), address);
    drop(lowered);
    assert_eq!(budget.used(), 0);
}

#[test]
fn partial_ir_failure_releases_allocations_without_disturbing_other_results() {
    let cancellation = CancellationToken::new();
    let source = Expr::Row(vec![
        Expr::Column("first".into()),
        Expr::Literal(Value::Bytes(vec![9; 16384])),
    ]);
    let budget = MemoryBudget::new(8192);
    let first = ExpressionPlan::lower_column_budgeted(
        &Expr::Literal(Value::Str("held".into())),
        &budget,
        &cancellation,
        &cancellation,
    )
    .unwrap();
    let held = budget.used();
    assert!(matches!(
        ExpressionPlan::lower_column_budgeted(&source, &budget, &cancellation, &cancellation),
        Err(CatalogRetentionError::Memory(MemoryError::Limit { .. }))
    ));
    assert_eq!(budget.used(), held);
    assert!(budget.peak() > held);
    assert_eq!(*first, ScalarExpr::Literal(Value::Str("held".into())));
    let Expr::Row(items) = source else {
        panic!("row source");
    };
    assert_eq!(items[1], Expr::Literal(Value::Bytes(vec![9; 16384])));
    drop(first);
    assert_eq!(budget.used(), 0);
}

#[test]
fn original_and_invoking_cancellation_abort_before_and_during_container_production() {
    for cancel_original in [true, false] {
        let budget = MemoryBudget::new(4096);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let token = if cancel_original {
            &original
        } else {
            &invoking
        };
        let mut lowering = Lowering {
            control: Some(Control::new(&budget, &original, &invoking)),
        };
        let mut count = 0;
        let result = lowering.map([1, 2, 3].into_iter(), |_, value| {
            count += 1;
            token.cancel();
            Ok(value)
        });
        assert!(matches!(result, Err(CatalogRetentionError::Cancelled(_))));
        assert_eq!(count, 1);
        drop(lowering);
        assert_eq!(budget.used(), 0);
        assert!(matches!(
            ExpressionPlan::lower_column_budgeted(
                &Expr::Literal(Value::Null),
                &budget,
                &original,
                &invoking
            ),
            Err(CatalogRetentionError::Cancelled(_))
        ));
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn validated_column_subquery_errors_cleanup_and_inline_literals_need_no_headroom() {
    let cancellation = CancellationToken::new();
    let budget = MemoryBudget::new(1 << 20);
    for sql in [
        "SELECT (SELECT 1)",
        "SELECT EXISTS (SELECT 1)",
        "SELECT 1 IN (SELECT 1)",
    ] {
        let source = expression(sql);
        assert!(matches!(
            ExpressionPlan::lower_column_budgeted(&source, &budget, &cancellation, &cancellation),
            Err(CatalogRetentionError::UnexpectedSubquery)
        ));
        assert_eq!(budget.used(), 0);
        assert_eq!(ExpressionPlan::lower(source).subqueries.len(), 1);
    }
    let zero = MemoryBudget::new(0);
    for source in [
        Expr::Literal(Value::Null),
        Expr::Literal(Value::Int(3)),
        Expr::Array(Vec::new()),
        Expr::Param(1),
    ] {
        let result =
            ExpressionPlan::lower_column_budgeted(&source, &zero, &cancellation, &cancellation)
                .unwrap();
        assert_eq!(result.reserved_bytes(), 0);
    }
    assert_eq!(zero.peak(), 0);
}

#[test]
fn retained_ir_lease_equals_destination_containers_and_leaf_payloads() {
    let source = Expr::Row(vec![
        Expr::Column("column".into()),
        Expr::Cast {
            expr: Box::new(Expr::Literal(Value::Str("datum".into()))),
            ty: "text".into(),
        },
    ]);
    let budget = MemoryBudget::new(4096);
    let cancellation = CancellationToken::new();
    let lowered =
        ExpressionPlan::lower_column_budgeted(&source, &budget, &cancellation, &cancellation)
            .unwrap();
    let ScalarExpr::Row(items) = &*lowered else {
        panic!("row IR");
    };
    let ScalarExpr::Column(name) = &items[0] else {
        panic!("column IR");
    };
    let ScalarExpr::Cast { expr, ty } = &items[1] else {
        panic!("cast IR");
    };
    let ScalarExpr::Literal(Value::Str(value)) = expr.as_ref() else {
        panic!("literal IR");
    };
    assert_eq!(
        lowered.reserved_bytes(),
        items.capacity() * size_of::<ScalarExpr>()
            + name.capacity()
            + size_of::<ScalarExpr>()
            + ty.capacity()
            + value.capacity()
    );
    assert_eq!(budget.peak(), lowered.reserved_bytes());
    drop(lowered);
    assert_eq!(budget.used(), 0);
}
