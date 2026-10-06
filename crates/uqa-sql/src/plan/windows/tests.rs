//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn query(sql: &str) -> QueryPlan {
    let UnifiedPlan::Query(query) = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0))
    else {
        panic!("query")
    };
    *query
}

#[test]
fn canonical_input_changes_refresh_inherited_calls_without_orphan_subqueries() {
    let mut query = query("SELECT sum(v) OVER child, row_number() OVER base FROM t WINDOW base AS (ORDER BY (SELECT 'now'::timestamp)), child AS (base ROWS CURRENT ROW)");
    let RelationalPlan::QueryBlock(block) = &mut query.root else {
        panic!("block")
    };
    assert_eq!(block.subqueries.len(), 1);
    block.windows[0].spec.order_by[0].expr = ScalarExpr::Literal(uqa_core::Value::Int(7));
    query.normalize_window_definitions().unwrap();
    let RelationalPlan::QueryBlock(block) = &query.root else {
        panic!("block")
    };
    assert!(block.subqueries.is_empty());
    for projection in &block.projections {
        let ScalarExpr::WindowCall { spec, .. } = &projection.expr else {
            panic!("window")
        };
        assert_eq!(
            spec.order_by[0].expr,
            ScalarExpr::Literal(uqa_core::Value::Int(7))
        );
    }
}

#[test]
fn nested_queries_keep_independent_canonical_window_slots() {
    let mut query = query("SELECT row_number() OVER w, (SELECT row_number() OVER w WINDOW w AS (ORDER BY 2)) WINDOW w AS (ORDER BY 1)");
    query.normalize_window_definitions().unwrap();
    let RelationalPlan::QueryBlock(block) = &query.root else {
        panic!("block")
    };
    let ScalarExpr::WindowCall { spec, .. } = &block.projections[0].expr else {
        panic!("window")
    };
    assert_eq!(
        spec.order_by[0].expr,
        ScalarExpr::Literal(uqa_core::Value::Int(1))
    );
    let RelationalPlan::QueryBlock(child) = &block.subqueries[0].root else {
        panic!("child")
    };
    let ScalarExpr::WindowCall { spec, .. } = &child.projections[0].expr else {
        panic!("child window")
    };
    assert_eq!(
        spec.order_by[0].expr,
        ScalarExpr::Literal(uqa_core::Value::Int(2))
    );
}

#[test]
fn unused_window_aggregate_marks_the_query_as_an_aggregate() {
    let query = query("SELECT 1 FROM t WINDOW unused AS (ORDER BY sum(v))");
    let RelationalPlan::QueryBlock(block) = &query.root else {
        panic!("block")
    };
    assert!(matches!(block.compute, crate::plan::ComputePlan::Aggregate));
}

#[test]
fn invalid_canonical_slots_and_inheritance_are_rejected() {
    let mut query = query("SELECT row_number() OVER w WINDOW w AS (ORDER BY 1)");
    let RelationalPlan::QueryBlock(block) = &mut query.root else {
        panic!("block")
    };
    block.windows[0].inherited = Some(0);
    assert!(query
        .normalize_window_definitions()
        .unwrap_err()
        .to_string()
        .contains("earlier named"));
    let RelationalPlan::QueryBlock(block) = &mut query.root else {
        panic!("block")
    };
    block.windows[0].inherited = None;
    let ScalarExpr::WindowCall { spec, .. } = &mut block.projections[0].expr else {
        panic!("window")
    };
    spec.definition = Some(1);
    assert!(query
        .normalize_window_definitions()
        .unwrap_err()
        .to_string()
        .contains("missing window"));
}
