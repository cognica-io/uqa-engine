//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ast::{FunctionBinding, FunctionDispatch, FunctionOrderSyntax},
    plan::{QueryPlan, RelationalPlan},
};
use uqa_core::Value;

fn query(sql: &str) -> QueryPlan {
    let statement = crate::compile(sql).unwrap().remove(0);
    let crate::plan::UnifiedPlan::Query(query) = crate::plan::UnifiedPlan::lower(statement) else {
        panic!("query");
    };
    *query
}

fn symmetric(args: Vec<ScalarExpr>) -> ScalarExpr {
    let binding = FunctionBinding::dispatched(FunctionDispatch::BetweenSymmetric);
    ScalarExpr::Func {
        name: binding.name.clone(),
        binding: Some(binding),
        args,
        distinct: false,
        order_by: Vec::new(),
        filter: None,
        order_syntax: FunctionOrderSyntax::Ordinary,
    }
}

#[test]
fn restored_comparisons_copy_only_repeated_subquery_occurrences() {
    let mut query = query("SELECT (SELECT 1), (SELECT 2), (SELECT 3)");
    let RelationalPlan::QueryBlock(block) = &mut query.root else {
        panic!("query block");
    };
    block.projections[0].expr = symmetric((0..3).map(ScalarExpr::ScalarSubquery).collect());
    block.projections.truncate(1);
    assert!(restore_query(&mut query).unwrap());
    let RelationalPlan::QueryBlock(block) = &query.root else {
        panic!("query block");
    };
    let mut slots = Vec::new();
    block.projections[0].expr.visit(&mut |node| {
        if let ScalarExpr::ScalarSubquery(slot) = node {
            slots.push(*slot);
        }
    });
    assert_eq!(slots, [0, 1, 3, 2, 4, 5, 6, 7]);
    assert_eq!(block.subqueries.len(), 8);
    for (original, copy) in [(0, 3), (0, 4), (0, 6), (1, 7), (2, 5)] {
        assert_eq!(
            serde_json::to_value(&block.subqueries[original]).unwrap(),
            serde_json::to_value(&block.subqueries[copy]).unwrap()
        );
    }
    let before = serde_json::to_value(&query).unwrap();
    assert!(!restore_query(&mut query).unwrap());
    assert_eq!(serde_json::to_value(&query).unwrap(), before);
}

#[test]
fn restoration_reaches_nested_query_arenas_before_copying_them() {
    let mut query = query("SELECT (SELECT (SELECT 1))");
    let RelationalPlan::QueryBlock(block) = &mut query.root else {
        panic!("query block");
    };
    let RelationalPlan::QueryBlock(child) = &mut block.subqueries[0].root else {
        panic!("child query");
    };
    child.projections[0].expr = ScalarExpr::Between {
        expr: Box::new(ScalarExpr::ScalarSubquery(0)),
        low: Box::new(ScalarExpr::Literal(Value::Int(0))),
        high: Box::new(ScalarExpr::Literal(Value::Int(2))),
    };
    block.projections[0].expr = symmetric(vec![
        ScalarExpr::ScalarSubquery(0),
        ScalarExpr::Literal(Value::Bool(false)),
        ScalarExpr::Literal(Value::Bool(true)),
    ]);
    assert!(restore_query(&mut query).unwrap());
    let RelationalPlan::QueryBlock(block) = &query.root else {
        panic!("query block");
    };
    assert_eq!(block.subqueries.len(), 4);
    for child in &block.subqueries {
        let RelationalPlan::QueryBlock(child) = &child.root else {
            panic!("child query");
        };
        assert_eq!(child.subqueries.len(), 2);
        assert!(matches!(child.projections[0].expr, ScalarExpr::And(_)));
    }
}

#[test]
fn invalid_subquery_references_fail_without_replacing_the_expression() {
    let mut expression = ScalarExpr::Between {
        expr: Box::new(ScalarExpr::ScalarSubquery(0)),
        low: Box::new(ScalarExpr::Literal(Value::Int(0))),
        high: Box::new(ScalarExpr::Literal(Value::Int(1))),
    };
    let before = expression.clone();
    assert!(restore_node(&mut expression, &mut Vec::new()).is_err());
    assert_eq!(expression, before);
}

#[test]
fn user_function_identity_cannot_be_rewritten_as_comparison_syntax() {
    let mut expression = symmetric(vec![ScalarExpr::Literal(Value::Null); 3]);
    let ScalarExpr::Func {
        binding: Some(binding),
        ..
    } = &mut expression
    else {
        unreachable!();
    };
    binding.builtin = false;
    let before = expression.clone();
    assert!(!restore_node(&mut expression, &mut Vec::new()).unwrap());
    assert_eq!(expression, before);
}

#[test]
fn restored_syntax_gives_every_subquery_occurrence_an_independent_slot() {
    use crate::ast::{Expr, Statement};
    let Statement::Select(query) = crate::compile("SELECT 7").unwrap().remove(0) else {
        panic!("query")
    };
    let operand = Expr::ScalarSubquery(query);
    let binding = FunctionBinding::dispatched(FunctionDispatch::BetweenSymmetric);
    let mut expression = Expr::Func {
        name: binding.name.clone(),
        binding: Some(binding),
        args: vec![operand; 3],
        distinct: false,
        order_by: Vec::new(),
        filter: None,
        order_syntax: FunctionOrderSyntax::Ordinary,
    };
    assert!(expression.upgrade_legacy_serialized_dispatches());
    let before = expression.clone();
    assert!(!expression.upgrade_legacy_serialized_dispatches());
    assert_eq!(expression, before);
    let plan = crate::plan::ExpressionPlan::lower(expression);
    let mut slots = Vec::new();
    plan.scalar.visit(&mut |node| {
        if let ScalarExpr::ScalarSubquery(slot) = node {
            slots.push(*slot);
        }
    });
    assert_eq!(slots, (0..8).collect::<Vec<_>>());
    assert_eq!(plan.subqueries.len(), 8);
}

#[test]
fn syntax_restoration_keeps_a_selected_user_routine_and_its_binding() {
    let expression = symmetric(vec![ScalarExpr::Literal(Value::Int(4)); 3]);
    let mut json = serde_json::to_value(expression).unwrap();
    json["Func"]["binding"]["builtin"] = serde_json::json!(false);
    let mut expression: crate::ast::Expr = serde_json::from_value(json).unwrap();
    let before = expression.clone();
    assert!(!expression.upgrade_legacy_serialized_dispatches());
    assert_eq!(expression, before);
}
