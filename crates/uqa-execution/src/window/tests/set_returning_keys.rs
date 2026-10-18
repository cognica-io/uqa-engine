//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_sql::ast::BinaryOp;

fn series(high: i64) -> ScalarExpr {
    ScalarExpr::Func {
        order_syntax: uqa_sql::ast::FunctionOrderSyntax::Ordinary,
        name: "generate_series".into(),
        binding: None,
        args: vec![
            ScalarExpr::Literal(Value::Int(1)),
            ScalarExpr::Literal(Value::Int(high)),
        ],
        distinct: false,
        order_by: Vec::new(),
        filter: None,
    }
}

fn window_call(
    name: &str,
    partition_by: Vec<ScalarExpr>,
    order_by: Vec<ScalarOrder>,
) -> ScalarExpr {
    ScalarExpr::WindowCall {
        name: name.into(),
        args: Vec::new(),
        spec: ScalarWindowSpec {
            definition: None,
            partition_by,
            order_by,
            frame: None,
        },
        filter: None,
        modifiers: uqa_sql::ast::WindowCallModifiers::default(),
    }
}

fn plus_zero(expression: ScalarExpr) -> ScalarExpr {
    ScalarExpr::Binary {
        op: BinaryOp::Add,
        lhs: Box::new(expression),
        rhs: Box::new(ScalarExpr::Literal(Value::Int(0))),
    }
}

fn projection(expr: ScalarExpr) -> ProjectionPlan {
    ProjectionPlan { expr, alias: None }
}

#[test]
fn target_list_calls_that_repeat_a_set_returning_key_read_its_column() {
    // SELECT generate_series(1,2), generate_series(1,2) + 0, generate_series(1,3), row_number() OVER (PARTITION BY generate_series(1,2)), rank() OVER (ORDER BY generate_series(1,2) DESC)
    let descending = ScalarOrder {
        expr: series(2),
        descending: true,
        nulls: None,
    };
    let mut plan = prepare_window_plan(&[
        projection(series(2)),
        projection(plus_zero(series(2))),
        projection(series(3)),
        projection(window_call("row_number", vec![series(2)], Vec::new())),
        projection(window_call("rank", Vec::new(), vec![descending])),
    ]);
    let keys = plan
        .take_set_returning_keys(&mut |expression| {
            Ok(matches!(expression, ScalarExpr::Func { name, .. } if name == "generate_series"))
        })
        .unwrap();
    // Both windows share the one expanded key, which the input computes below the window.
    assert_eq!(keys.len(), 1);
    let (column, key) = keys[0].clone();
    assert_eq!(key, series(2));
    let read = ScalarExpr::InternalColumn(column);
    for pass in &plan.passes {
        let pass_keys = pass
            .partition_by
            .iter()
            .chain(pass.order_by.iter().map(|order| &order.expr));
        assert!(pass_keys.into_iter().all(|expression| expression == &read));
    }
    // The target list reads the key's column wherever it repeats the call, so the rows are not expanded again above the window; a different call is still evaluated there.
    let projections = plan
        .projections()
        .iter()
        .map(|projection| projection.expr.clone())
        .collect::<Vec<_>>();
    assert_eq!(projections[0], read);
    assert_eq!(projections[1], plus_zero(read.clone()));
    assert_eq!(projections[2], series(3));
    // An ORDER BY or DISTINCT ON expression appended above the window reads the key the same way.
    let mut order = ScalarExpr::UnaryMinus(Box::new(series(2)));
    read_set_returning_keys(&mut order, &keys);
    assert_eq!(order, ScalarExpr::UnaryMinus(Box::new(read)));
    let mut other = series(3);
    read_set_returning_keys(&mut other, &keys);
    assert_eq!(other, series(3));
}
