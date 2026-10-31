//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    catalog::test_support::{empty_scope, NoRoutines},
    query::{binding::sources::bind_query_block_with_sources, privileges::SELECT_CHECKS},
};
use uqa_core::memory::ProductionControl;
use uqa_sql::binding::ExecutionBindingOptions;
use uqa_sql::plan::{QueryPlan, RelationalPlan, UnifiedPlan};

static ROUTINES: NoRoutines = NoRoutines;

fn query(sql: &str) -> QueryPlan {
    let UnifiedPlan::Query(query) = UnifiedPlan::lower(uqa_sql::compile(sql).unwrap().remove(0))
    else {
        panic!("query")
    };
    *query
}
fn block(query: &QueryPlan) -> &QueryBlockPlan {
    let RelationalPlan::QueryBlock(block) = &query.root else {
        panic!("block")
    };
    block
}
fn child(block: &QueryBlockPlan) -> &QueryBlockPlan {
    let Some(SourcePlan::Subquery { body, .. }) = &block.from else {
        panic!("derived")
    };
    self::block(body)
}
fn bindings<'a>(block: &'a QueryBlockPlan, scope: &CteScope) -> SourceBindings<'a> {
    bind_query_block_with_sources(
        &ROUTINES,
        block,
        &[],
        scope,
        ExecutionBindingOptions::default(),
        None,
        &ProductionControl::uncontrolled(),
    )
    .unwrap()
    .sources
    .unwrap()
}
fn enter_blocks<'a>(
    block: &'a QueryBlockPlan,
    inherited: Option<&SourceBindings<'a>>,
    scope: &CteScope,
    reuse: bool,
) {
    let mut bound = bind_query_block_with_sources(
        &ROUTINES,
        block,
        &[],
        scope,
        ExecutionBindingOptions::default(),
        inherited,
        &ProductionControl::uncontrolled(),
    )
    .unwrap();
    if !reuse {
        if let Some(bindings) = bound.sources.as_mut() {
            bindings.authorization = None;
        }
    }
    let source = block.from.as_ref().unwrap();
    let checked = ensure_select_with_sources(block, source, scope, bound.sources.as_ref()).unwrap();
    if matches!(source, SourcePlan::Subquery { .. }) {
        enter_blocks(child(block), checked.as_ref(), scope, reuse);
    }
}

#[test]
fn nested_select_coverage_checks_complete_lineage_once() {
    let scope = empty_scope();
    for depth in [1, 8, 32] {
        let mut plan = query("SELECT value FROM (VALUES (7)) AS input(value)");
        for _ in 0..depth {
            let mut parent = query("SELECT value FROM (SELECT 0 AS value) AS nested OFFSET 0");
            let RelationalPlan::QueryBlock(parent_block) = &mut parent.root else {
                unreachable!()
            };
            let Some(SourcePlan::Subquery { body, .. }) = &mut parent_block.from else {
                unreachable!()
            };
            body.root = plan.root;
            plan = parent;
        }
        SELECT_CHECKS.with(|count| count.set(0));
        enter_blocks(block(&plan), None, &scope, false);
        assert_eq!(SELECT_CHECKS.with(std::cell::Cell::get), depth + 1);
        SELECT_CHECKS.with(|count| count.set(0));
        enter_blocks(block(&plan), None, &scope, true);
        assert_eq!(SELECT_CHECKS.with(std::cell::Cell::get), 1, "depth {depth}");
    }
}

#[test]
fn select_coverage_requires_the_same_role_incarnation_and_lookup_mode() {
    use std::sync::Arc;
    use uqa_sql::catalog::roles::identity::RoleBinding;
    let role = |id| RoleReference::Bound(Arc::new(RoleBinding::new("reader".into(), 42, [id; 16])));
    let mut original = empty_scope();
    let original = original.enter_privilege_subject(role(1));
    let plan = query("SELECT * FROM (SELECT * FROM (VALUES (7)) AS input(value)) AS derived");
    let root = block(&plan);
    let bound = bindings(root, &original);
    let checked =
        ensure_select_with_sources(root, root.from.as_ref().unwrap(), &original, Some(&bound))
            .unwrap()
            .unwrap();
    let nested = child(root);
    let (_, inherited) = checked
        .query_source(&ROUTINES, nested, &[], &original, None, false)
        .unwrap();
    assert!(inherited.unwrap().authorization.is_some());
    let mut changed = original.clone();
    let changed = changed.enter_privilege_subject(role(2));
    let (_, inherited) = checked
        .query_source(&ROUTINES, nested, &[], &changed, None, false)
        .unwrap();
    assert!(inherited.unwrap().authorization.is_none());
    let mut changed = original.clone();
    changed
        .set_relation_lookup_mode(RelationLookupMode::Bound)
        .unwrap();
    let (_, inherited) = checked
        .query_source(&ROUTINES, nested, &[], &changed, None, false)
        .unwrap();
    assert!(inherited.unwrap().authorization.is_none());
}

#[test]
fn changed_query_projection_and_catalog_require_new_select_checks() {
    let original = empty_scope();
    let plan = query("SELECT * FROM (SELECT * FROM (VALUES (7)) AS input(value)) AS derived");
    let root = block(&plan);
    let bound = bindings(root, &original);
    let checked =
        ensure_select_with_sources(root, root.from.as_ref().unwrap(), &original, Some(&bound))
            .unwrap()
            .unwrap();
    let nested = child(root);
    let (_, inherited) = checked
        .query_source(&ROUTINES, nested, &[], &original, None, false)
        .unwrap();
    let inherited = inherited.unwrap();
    SELECT_CHECKS.with(|count| count.set(0));
    ensure_select_with_sources(
        nested,
        nested.from.as_ref().unwrap(),
        &original,
        Some(&inherited),
    )
    .unwrap();
    assert_eq!(SELECT_CHECKS.with(std::cell::Cell::get), 0);
    let copied = nested.clone();
    ensure_select_with_sources(
        &copied,
        copied.from.as_ref().unwrap(),
        &original,
        Some(&inherited),
    )
    .unwrap();
    assert_eq!(SELECT_CHECKS.with(std::cell::Cell::get), 1);
    ensure_select_with_sources(
        nested,
        nested.from.as_ref().unwrap(),
        &empty_scope(),
        Some(&inherited),
    )
    .unwrap();
    assert_eq!(SELECT_CHECKS.with(std::cell::Cell::get), 2);
}
