//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::{
    bind_query_block_with_sources_with_control, ControlledExecutionBlock, ExecutionBindingOptions,
    ExecutionSourceBindings, SharedExecutionSourceBindings,
};
use super::{block, Routines};
use crate::{
    binding::fixture::empty_binding_context,
    plan::{QueryBlockPlan, RelationalPlan, SourcePlan},
    ColumnType, RowSchema, SQLError, ScalarExpr,
};
use uqa_core::{
    memory::{MemoryBudget, Produced, ProductionControl},
    CancellationToken, Value,
};

fn bind<'a>(
    query: &'a QueryBlockPlan,
    deferred: bool,
    control: &ProductionControl<'_>,
) -> Result<ControlledExecutionBlock<'a>, SQLError> {
    bind_query_block_with_sources_with_control(
        &Routines,
        query,
        &[],
        empty_binding_context().copy_with_control(control)?,
        ExecutionBindingOptions {
            outer: None,
            defer_distinct_limit: deferred,
        },
        control,
    )
}

#[test]
fn controlled_execution_binding_admits_private_syntax_and_retires_it_before_shared_schema() {
    let mut query =
        block("SELECT DISTINCT 1 AS payload FROM generate_series(1,2) AS input(value) LIMIT 1");
    query.projections[0].expr = ScalarExpr::Literal(Value::Str("x".repeat(4 << 20)));
    let budget = MemoryBudget::new(32 << 20);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    let occupied = budget.reserve(budget.available() - (64 << 10)).unwrap();
    let mut error = None;
    let allocation = allocation_counter::measure(|| error = bind(&query, true, &control).err());
    assert_eq!(error.unwrap().sqlstate(), Some("53200"));
    assert!(allocation.bytes_total < 256 << 10, "{allocation:?}");
    assert_eq!(budget.used(), occupied.bytes());
    drop(occupied);
    let bound = bind(&query, true, &control).unwrap();
    assert!(bound.block.limit.is_none());
    assert!(query.limit.is_some());
    assert!(budget.used() >= 4 << 20);
    assert_eq!(bound.schema.column_type(0), Some(&ColumnType::Integer));
    let schema = bound.schema.clone();
    drop(bound);
    assert!(budget.used() > 0 && budget.used() < 64 << 10);
    drop(schema);
    assert_eq!(budget.used(), 0);
    for sql in [
        "SELECT DISTINCT 1",
        "SELECT DISTINCT 1 LIMIT 1",
        "SELECT DISTINCT 1 OFFSET 1",
        "SELECT DISTINCT 1 LIMIT 1 OFFSET 1",
    ] {
        let query = block(sql);
        let bound = bind(&query, true, &control).unwrap();
        assert!(bound.block.limit.is_none() && bound.block.offset.is_none());
        drop(bound);
        assert_eq!(budget.used(), 0);
    }
    for token in [&original, &invoking] {
        token.cancel();
        assert_eq!(
            bind(&query, true, &control).err().unwrap().sqlstate(),
            Some("57014")
        );
        assert_eq!(budget.used(), 0);
        token.reset();
    }
    let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _bound = bind(&query, true, &control).unwrap();
        panic!("retire private execution binding");
    }));
    assert!(unwind.is_err());
    assert_eq!(budget.used(), 0);
}

#[test]
fn controlled_function_group_binding_retires_temporary_members_and_preserves_ordinality() {
    let query = block(&format!("SELECT * FROM ROWS FROM (generate_series(1,2), unnest(ARRAY['{}'])) WITH ORDINALITY AS input(g,u,n)", "x".repeat(4 << 20)));
    let budget = MemoryBudget::new(32 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let bound = bind(&query, false, &control).unwrap();
    let Some(SourcePlan::FunctionGroup {
        functions,
        ordinality,
        alias,
        ..
    }) = &bound.block.from
    else {
        panic!("function group")
    };
    assert!(*ordinality);
    assert_eq!(alias.as_deref(), Some("input"));
    assert!(functions.iter().all(|function| function
        .binding
        .as_ref()
        .is_some_and(|binding| binding.builtin)));
    assert_eq!(bound.schema.column_type(0), Some(&ColumnType::Integer));
    assert_eq!(bound.schema.column_type(1), Some(&ColumnType::Text));
    assert_eq!(bound.schema.column_type(2), Some(&ColumnType::BigInteger));
    assert!(
        budget.used() >= 4 << 20 && budget.used() < (4 << 20) + (128 << 10),
        "temporary member remains live: {}",
        budget.used()
    );
    let Some(SourcePlan::FunctionGroup { functions, .. }) = &query.from else {
        panic!("original function group")
    };
    assert!(functions.iter().all(|function| function.binding.is_none()));
    drop(bound);
    assert_eq!(budget.used(), 0);
}

fn derived(source: &SourcePlan) -> &QueryBlockPlan {
    let SourcePlan::Subquery { body, .. } = source else {
        panic!("derived source")
    };
    let RelationalPlan::QueryBlock(block) = &body.root else {
        panic!("derived block")
    };
    block
}

#[test]
fn captured_source_metadata_admits_exact_nodes_and_releases_independent_readers() {
    use super::super::CapturedSources;
    let query = block("SELECT l.x,r.y FROM (SELECT 1 AS x) AS l CROSS JOIN (SELECT 2 AS y) AS r");
    let source = query.from.as_ref().unwrap();
    let SourcePlan::Join { left, right, .. } = source else {
        panic!("join")
    };
    let schema = RowSchema::new(vec!["raw".into()]);
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let mut captured = CapturedSources::for_source(source, &control).unwrap();
    assert!(budget.used() > 0);
    captured.record(derived(left), &schema, None);
    captured.record(derived(right), &schema, None);
    let retained = captured.take(source, &control).unwrap();
    drop(captured);
    let header = size_of::<Produced<ExecutionSourceBindings<'_>>>();
    let children = size_of::<(
        SharedExecutionSourceBindings<'_>,
        SharedExecutionSourceBindings<'_>,
    )>();
    assert_eq!(budget.used(), 3 * header + children);
    let reader = retained.join_child(false).unwrap();
    assert!(reader.matches_source(left));
    assert_eq!(
        reader.query_source(derived(left), None, false).unwrap().0,
        schema
    );
    assert!(reader.query_source(derived(right), None, false).is_none());
    assert!(reader.query_source(derived(left), None, true).is_none());
    drop(retained);
    assert_eq!(budget.used(), header);
    drop(reader);
    assert_eq!(budget.used(), 0);
    let occupied = budget.reserve(budget.available() - 1).unwrap();
    let allocation = allocation_counter::measure(|| {
        assert_eq!(
            CapturedSources::for_source(source, &control)
                .err()
                .unwrap()
                .sqlstate(),
            Some("53200")
        );
    });
    assert!(allocation.bytes_total < 4096, "{allocation:?}");
    assert_eq!(budget.used(), occupied.bytes());
    drop(occupied);
    let mut captured = CapturedSources::for_source(source, &control).unwrap();
    token.cancel();
    assert_eq!(
        captured.take(source, &control).err().unwrap().sqlstate(),
        Some("57014")
    );
    drop(captured);
    assert_eq!(budget.used(), 0);
}

#[test]
fn controlled_grouping_name_binding_preserves_outer_precedence_and_independent_scalar_owners() {
    let mut query = block("SELECT 'small'::text AS key GROUP BY key");
    query.projections[0].expr = ScalarExpr::Literal(Value::Str("x".repeat(4 << 20)));
    let budget = MemoryBudget::new(32 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let input = query.copy_for_rewrite(&control).unwrap();
    let schema = RowSchema::with_types(vec!["key".into()], vec![Some(ColumnType::Integer)]);
    let output = crate::semantics::grouping_sets::bind_grouping_names_owned(
        &Routines,
        input,
        &schema,
        None,
        &[],
        &control,
    )
    .unwrap();
    let RelationalPlan::QueryBlock(block) = &output.root else {
        panic!("block")
    };
    assert!(matches!(block.group_by[0], ScalarExpr::Column(_)));
    assert!(budget.used() < (4 << 20) + (128 << 10));
    drop(output);
    assert_eq!(budget.used(), 0);
    let input = query.copy_for_rewrite(&control).unwrap();
    let occupied = budget.reserve(budget.available() - (64 << 10)).unwrap();
    let error = crate::semantics::grouping_sets::bind_grouping_names_owned(
        &Routines,
        input,
        &schema,
        Some(&schema),
        &[],
        &control,
    )
    .err()
    .unwrap();
    assert_eq!(error.sqlstate(), Some("53200"));
    assert_eq!(budget.used(), occupied.bytes());
    drop(occupied);
    let input = query.copy_for_rewrite(&control).unwrap();
    let output = crate::semantics::grouping_sets::bind_grouping_names_owned(
        &Routines,
        input,
        &schema,
        Some(&schema),
        &[],
        &control,
    )
    .unwrap();
    let RelationalPlan::QueryBlock(block) = &output.root else {
        panic!("block")
    };
    assert!(
        matches!(&block.group_by[0], ScalarExpr::Literal(Value::Str(value)) if value.len() == 4 << 20)
    );
    assert!(budget.used() >= 8 << 20);
    drop(output);
    assert_eq!(budget.used(), 0);
}
