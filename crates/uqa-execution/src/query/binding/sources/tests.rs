//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::test_support::{empty_scope, NoRoutines};
use uqa_sql::{
    plan::{RelationalPlan, UnifiedPlan},
    ColumnType,
};

fn query(sql: &str) -> uqa_sql::plan::QueryPlan {
    let UnifiedPlan::Query(query) = UnifiedPlan::lower(uqa_sql::compile(sql).unwrap().remove(0))
    else {
        panic!("query");
    };
    *query
}

fn block(query: &uqa_sql::plan::QueryPlan) -> &QueryBlockPlan {
    let RelationalPlan::QueryBlock(block) = &query.root else {
        panic!("query block");
    };
    block
}

fn child(block: &QueryBlockPlan) -> &QueryBlockPlan {
    let Some(SourcePlan::Subquery { body, .. }) = &block.from else {
        panic!("derived source");
    };
    self::block(body)
}

#[test]
fn source_transfer_rebinds_changed_parameters_and_cte_definitions() {
    let routines = NoRoutines;
    let scope = empty_scope();
    let query_plan =
        query("SELECT * FROM (SELECT * FROM (VALUES ($1)) AS input(value)) AS derived");
    let root = block(&query_plan);
    let integers = [SQLParam::typed_scalar(
        uqa_core::Value::Int(7),
        ColumnType::Integer,
    )];
    let strings = [SQLParam::typed_scalar(
        uqa_core::Value::Str("seven".into()),
        ColumnType::Text,
    )];
    let bound =
        bind_query_block_with_sources(&routines, root, &integers, &scope, None, false, None)
            .unwrap();
    let bindings = bound.sources.as_ref().unwrap();
    assert!(bindings
        .query_source(&routines, child(root), &integers, &scope, None, false)
        .is_some());
    assert!(bindings
        .query_source(&routines, child(root), &strings, &scope, None, false)
        .is_none());
    let rebound = bind_query_block_with_sources(
        &routines,
        child(root),
        &strings,
        &scope,
        None,
        false,
        Some(bindings),
    )
    .unwrap();
    assert_eq!(rebound.schema.column_type(0), Some(&ColumnType::Text));

    let mut scope = empty_scope();
    scope.insert_deferred(
        query("WITH input AS (SELECT 1 AS value) SELECT * FROM input")
            .ctes
            .remove(0)
            .into(),
    );
    let query_plan = query("SELECT * FROM (SELECT * FROM input) AS derived");
    let root = block(&query_plan);
    let bound =
        bind_query_block_with_sources(&routines, root, &[], &scope, None, false, None).unwrap();
    let bindings = bound.sources.as_ref().unwrap();
    assert!(bindings
        .query_source(&routines, child(root), &[], &scope, None, false)
        .is_some());
    scope.insert_deferred(
        query("WITH input AS (SELECT 'text'::text AS value) SELECT * FROM input")
            .ctes
            .remove(0)
            .into(),
    );
    assert!(bindings
        .query_source(&routines, child(root), &[], &scope, None, false)
        .is_none());
    let rebound = bind_query_block_with_sources(
        &routines,
        child(root),
        &[],
        &scope,
        None,
        false,
        Some(bindings),
    )
    .unwrap();
    assert_eq!(rebound.schema.column_type(0), Some(&ColumnType::Text));
}
