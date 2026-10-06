//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ast::{BinaryOp, Expr},
    plan::ExpressionPlan,
    RowSchema,
};
use uqa_core::{memory::MemoryBudget, CancellationToken};

fn call(name: &str, args: Vec<Expr>) -> Expr {
    Expr::Func {
        order_syntax: crate::ast::FunctionOrderSyntax::Ordinary,
        name: name.into(),
        binding: None,
        args,
        distinct: false,
        order_by: Vec::new(),
        filter: None,
    }
}

fn input(expression: &Expr, budget: &MemoryBudget) -> Produced<ScalarExpr> {
    let token = CancellationToken::new();
    ExpressionPlan::lower_column_budgeted(expression, budget, &token, &token)
        .unwrap()
        .into()
}

#[test]
fn controlled_binding_preserves_cast_common_type_and_selected_call_semantics() {
    let schema = RowSchema::with_types(
        vec!["small".into(), "real".into()],
        vec![Some(ColumnType::SmallInteger), Some(ColumnType::Real)],
    );
    let expressions = [
        call("pg_typeof", vec![Expr::Column("small".into())]),
        Expr::Cast {
            implicit: false,
            expr: Box::new(Expr::Column("real".into())),
            ty: "text".into(),
        },
        Expr::UnaryMinus(Box::new(Expr::Column("small".into()))),
        call(
            "coalesce",
            vec![Expr::Literal(Value::Null), Expr::Column("small".into())],
        ),
        call(
            "jsonb_strip_nulls",
            vec![Expr::Literal(Value::Str("{\"a\":null}".into()))],
        ),
        Expr::Array(vec![
            Expr::Literal(Value::Str("2".into())),
            Expr::Column("small".into()),
        ]),
        Expr::Case {
            base: Some(Box::new(Expr::Column("small".into()))),
            when: vec![(
                Expr::Literal(Value::Str("2".into())),
                Expr::Column("real".into()),
            )],
            else_branch: Some(Box::new(Expr::Literal(Value::Int(7)))),
        },
        Expr::Binary {
            op: BinaryOp::Add,
            lhs: Box::new(Expr::Column("real".into())),
            rhs: Box::new(Expr::Column("small".into())),
        },
    ];
    for expression in expressions {
        let budget = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let expected = bind_type_introspection(
            ExpressionPlan::lower(expression.clone()).scalar,
            &schema,
            &[],
        );
        let output = bind_type_introspection_with_control(
            input(&expression, &budget),
            &schema,
            &[],
            &control,
        )
        .unwrap();
        assert_eq!(*output, expected);
        assert_eq!(budget.used(), output.reserved_bytes());
        drop(output);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn controlled_binding_admits_new_ir_before_mutating_the_owned_root() {
    let schema = RowSchema::with_types(vec!["small".into()], vec![Some(ColumnType::SmallInteger)]);
    let expression = Expr::Row(vec![
        call("pg_typeof", vec![Expr::Column("small".into())]),
        Expr::Literal(Value::Str("sibling".repeat(100))),
    ]);
    for remaining in [0, 8, 64, 256, 1024] {
        let budget = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let input = input(&expression, &budget);
        let other = budget
            .reserve(budget.limit() - budget.used() - remaining)
            .unwrap();
        match bind_type_introspection_with_control(input, &schema, &[], &control) {
            Ok(output) => {
                let ScalarExpr::Row(items) = &*output else {
                    panic!("row remains a row")
                };
                assert!(
                    matches!(&items[0], ScalarExpr::Cast {ty, expr, .. } if ty == "regtype" && matches!(expr.as_ref(), ScalarExpr::Literal(Value::Str(name)) if name == "smallint"))
                );
                assert_eq!(
                    items[1],
                    ScalarExpr::Literal(Value::Str("sibling".repeat(100)))
                );
                assert_eq!(budget.used(), other.bytes() + output.reserved_bytes());
                drop(output);
            }
            Err(error) => assert_eq!(error.sqlstate(), Some("53200")),
        }
        assert_eq!(budget.used(), other.bytes());
        drop(other);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn controlled_binding_preserves_semantic_fallback_but_never_resource_failure() {
    let schema = RowSchema::default();
    let expression = Expr::Binary {
        op: BinaryOp::Add,
        lhs: Box::new(Expr::Literal(Value::Bool(true))),
        rhs: Box::new(Expr::Literal(Value::Int(1))),
    };
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let expected = bind_type_introspection(
        ExpressionPlan::lower(expression.clone()).scalar,
        &schema,
        &[],
    );
    let output =
        bind_type_introspection_with_control(input(&expression, &budget), &schema, &[], &control)
            .unwrap();
    assert_eq!(*output, expected);
    drop(output);
    assert_eq!(budget.used(), 0);
    for cancel_original in [false, true] {
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let input = input(&expression, &budget);
        if cancel_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        let control = ProductionControl::new(&budget, &original, &invoking);
        let error =
            bind_type_introspection_with_control(input, &schema, &[], &control).unwrap_err();
        assert_eq!(error.sqlstate(), Some("57014"));
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn controlled_binding_checks_root_allowance_before_noop_leaves() {
    let budget = MemoryBudget::new(1 << 16);
    let foreign = MemoryBudget::new(1 << 16);
    let token = CancellationToken::new();
    let input = input(&Expr::Literal(Value::Str("owned leaf".into())), &budget);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        bind_type_introspection_with_control(
            input,
            &RowSchema::default(),
            &[],
            &ProductionControl::new(&foreign, &token, &token),
        )
    }));
    assert!(result.is_err());
    assert_eq!(budget.used(), 0);
    assert_eq!(foreign.used(), 0);
}

#[test]
fn array_common_type_preserves_coercion_origin_through_controlled_binding() {
    let schema = RowSchema::with_types(
        vec!["small".into(), "large".into()],
        vec![Some(ColumnType::SmallInteger), Some(ColumnType::BigInteger)],
    );
    let array = |name: &str| Expr::Array(vec![Expr::Column(name.into())]);
    for explicit in [false, true] {
        let source = if explicit {
            Expr::Cast {
                implicit: false,
                expr: Box::new(array("small")),
                ty: "bigint[]".into(),
            }
        } else {
            array("small")
        };
        let expression = call("coalesce", vec![source, array("large")]);
        let budget = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let output = bind_type_introspection_with_control(
            input(&expression, &budget),
            &schema,
            &[],
            &control,
        )
        .unwrap();
        let ScalarExpr::Func { args, .. } = &*output else {
            panic!("common expression")
        };
        assert!(matches!(&args[0], ScalarExpr::Cast { implicit, ty, expr }
            if *implicit != explicit && ty == "bigint[]" && matches!(expr.as_ref(), ScalarExpr::Array(_))));
        assert!(matches!(&args[1], ScalarExpr::Array(_)));
        assert_eq!(budget.used(), output.reserved_bytes());
        drop(output);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn unknown_common_input_relabels_a_domain_but_identical_domains_survive() {
    let domain = ColumnType::Domain {
        schema: "public".into(),
        name: "oid_domain".into(),
        oid: 90_001,
        array_oid: None,
        base: Box::new(ColumnType::Oid),
    };
    let schema = RowSchema::with_types(vec!["domain_value".into()], vec![Some(domain)]);
    for unknown in [false, true] {
        let column = Expr::Column("domain_value".into());
        let second = if unknown {
            Expr::Literal(Value::Null)
        } else {
            column.clone()
        };
        let expression = ExpressionPlan::lower(call("coalesce", vec![column, second])).scalar;
        let bound = bind_type_introspection(expression, &schema, &[]);
        let ScalarExpr::Func { args, .. } = bound else {
            panic!("common expression")
        };
        if unknown {
            assert!(matches!(&args[0], ScalarExpr::Cast { implicit: true, ty, .. } if ty == "oid"));
            assert!(matches!(&args[1], ScalarExpr::Cast { implicit: true, ty, .. } if ty == "oid"));
        } else {
            assert!(args
                .iter()
                .all(|argument| matches!(argument, ScalarExpr::Column(_))));
        }
    }
}
