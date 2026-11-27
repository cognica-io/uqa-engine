//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::test_support::{empty_scope, NoRoutines};
use uqa_core::memory::ProductionControl;
use uqa_sql::binding::ExecutionBindingOptions;
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
    let bound = bind_query_block_with_sources(
        &routines,
        root,
        &integers,
        &scope,
        ExecutionBindingOptions::default(),
        None,
        &ProductionControl::uncontrolled(),
    )
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
        ExecutionBindingOptions::default(),
        Some(bindings),
        &ProductionControl::uncontrolled(),
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
    let bound = bind_query_block_with_sources(
        &routines,
        root,
        &[],
        &scope,
        ExecutionBindingOptions::default(),
        None,
        &ProductionControl::uncontrolled(),
    )
    .unwrap();
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
        ExecutionBindingOptions::default(),
        Some(bindings),
        &ProductionControl::uncontrolled(),
    )
    .unwrap();
    assert_eq!(rebound.schema.column_type(0), Some(&ColumnType::Text));
}

#[test]
fn query_block_binding_retains_the_outer_row_and_distinct_output_syntax() {
    let mut scope = empty_scope();
    scope.set_row_lock_outer_row(crate::OwnedPhysicalRow::new(
        RowSchema::with_qualified_types(
            "parent",
            vec!["v".into()],
            vec![Some(ColumnType::Integer)],
        ),
        crate::PhysicalRow::from_values(vec![uqa_core::Value::Int(7)]),
    ));
    let outer = scope.outer_row_binding().unwrap();
    let query = query("SELECT DISTINCT parent.v FROM (VALUES (1)) AS input(n) LIMIT 1");
    let original = block(&query);
    let control = ProductionControl::uncontrolled();
    let prepared =
        prepare_query_block(&NoRoutines, original, &[], &mut scope, None, &control).unwrap();
    assert!(Arc::ptr_eq(&outer, &scope.outer_row_binding().unwrap()));
    let physical = prepared.schema;
    let same = QueryBlockRewrites::resolve(
        ExecutionQueryBlock::borrowed(original),
        original,
        &physical,
        &control,
    )
    .unwrap();
    let (query, output) = same.blocks();
    assert!(std::ptr::eq(query, output));
    assert!(output.limit.is_some());

    let bound = bind_query_block_with_sources(
        &NoRoutines,
        original,
        &[],
        &scope,
        ExecutionBindingOptions {
            outer: Some(&outer.schema),
            defer_distinct_limit: true,
        },
        None,
        &control,
    )
    .unwrap();
    let different =
        QueryBlockRewrites::resolve(bound.block, original, &physical, &control).unwrap();
    let (input, output) = different.blocks();
    assert!(!std::ptr::eq(input, output));
    assert!(input.limit.is_none());
    assert!(output.limit.is_some());
    assert_eq!(outer.physical_value_at(0), Some(&uqa_core::Value::Int(7)));
}
