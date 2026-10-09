//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ast::{ColumnType, InternalRelationId},
    plan::RelationalPlan,
};
use uqa_core::Value;

fn expression(sql: &str) -> ExpressionPlan {
    let super::super::UnifiedPlan::Query(query) =
        super::super::UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0))
    else {
        panic!("query")
    };
    let RelationalPlan::QueryBlock(mut block) = query.root else {
        panic!("query block")
    };
    ExpressionPlan {
        scalar: block.projections.remove(0).expr,
        subqueries: block.subqueries,
    }
}

#[test]
fn replacement_subqueries_use_their_owning_arena_and_preserve_other_inputs() {
    let input = InternalRelationId::allocate().column(0);
    let other = InternalRelationId::allocate().column(0);
    let mut plan = expression("SELECT COALESCE((SELECT (SELECT $1) + (SELECT 9)), $1) + $2");
    let mut bind = |node: &mut ScalarExpr| match node {
        ScalarExpr::Param(1) => *node = ScalarExpr::InternalColumn(input),
        ScalarExpr::Param(2) => *node = ScalarExpr::InternalColumn(other),
        _ => {}
    };
    super::super::rewrite_scalar_expression(&mut plan.scalar, &mut bind);
    for query in &mut plan.subqueries {
        query.rewrite_scalar_expressions(&mut bind);
    }
    let replacement = expression("SELECT (SELECT 7)");
    substitute_expression_inputs(&mut plan, &BTreeMap::from([(input, replacement)])).unwrap();
    assert_eq!(plan.subqueries.len(), 2);
    let RelationalPlan::QueryBlock(outer) = &plan.subqueries[0].root else {
        panic!("outer")
    };
    assert_eq!(outer.subqueries.len(), 2);
    let RelationalPlan::QueryBlock(inner) = &outer.subqueries[0].root else {
        panic!("inner")
    };
    assert_eq!(inner.subqueries.len(), 1);
    assert_eq!(inner.projections[0].expr, ScalarExpr::ScalarSubquery(0));
    let RelationalPlan::QueryBlock(original) = &outer.subqueries[1].root else {
        panic!("original")
    };
    assert_eq!(
        original.projections[0].expr,
        ScalarExpr::Literal(Value::Int(9))
    );
    let mut remaining = Vec::new();
    plan.scalar.visit(&mut |node| {
        if let ScalarExpr::InternalColumn(column) = node {
            remaining.push(*column);
        }
    });
    assert_eq!(remaining, [other]);
}

#[test]
fn substitution_retains_unreached_errors_and_declared_input_types() {
    let input = InternalRelationId::allocate().column(0);
    let mut plan = expression("SELECT CASE WHEN true THEN false ELSE $1 > 0 END");
    super::super::rewrite_scalar_expression(&mut plan.scalar, &mut |node| {
        if matches!(node, ScalarExpr::Param(1)) {
            *node = ScalarExpr::InternalColumn(input);
        }
    });
    let fault = expression("SELECT 1 / 0");
    substitute_expression_inputs(&mut plan, &BTreeMap::from([(input, fault.clone())])).unwrap();
    let mut divisions = 0;
    plan.scalar.visit(&mut |node| {
        if *node == fault.scalar {
            divisions += 1;
        }
    });
    assert_eq!(divisions, 1);
    let typed = ScalarExpr::TypedLiteral {
        value: Value::Int(7),
        ty: "smallint".into(),
        bound_type: Some(ColumnType::SmallInteger),
        composite_source: None,
        parameter_index: None,
    };
    let mut plan = ExpressionPlan {
        scalar: ScalarExpr::InternalColumn(input),
        subqueries: Vec::new(),
    };
    substitute_expression_inputs(
        &mut plan,
        &BTreeMap::from([(
            input,
            ExpressionPlan {
                scalar: typed.clone(),
                subqueries: Vec::new(),
            },
        )]),
    )
    .unwrap();
    assert_eq!(plan.scalar, typed);
}
