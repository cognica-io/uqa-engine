//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::CteScope;
use uqa_sql::plan::{QueryPlan, UnifiedPlan};

fn query(sql: &str) -> QueryPlan {
    let UnifiedPlan::Query(plan) = UnifiedPlan::lower(uqa_sql::compile(sql).unwrap().remove(0))
    else {
        panic!("expected query");
    };
    *plan
}

#[test]
fn execution_scope_copies_do_not_duplicate_immutable_plan_payloads() {
    let text = "x".repeat(128 * 1024);
    let mut definition = query(&format!(
        "WITH source AS (SELECT '{text}'::text AS value) SELECT value FROM source"
    ));
    let mut scope = CteScope::<()>::new();
    scope.insert_deferred(definition.ctes.remove(0));
    scope.scalar_subqueries = vec![query(&format!("SELECT '{text}'::text"))].into();

    let allocation = allocation_counter::measure(|| {
        let copies = (0..16).map(|_| scope.clone()).collect::<Vec<_>>();
        assert_eq!(copies.len(), 16);
        for copy in &copies {
            assert!(copy.deferred_reference("source").is_some());
            assert_eq!(copy.scalar_subqueries.len(), 1);
        }
    });
    assert!(
        allocation.bytes_total < 256 * 1024 && allocation.bytes_max < 128 * 1024,
        "execution scope copies duplicate immutable plans: {allocation:?}"
    );

    let mut child = scope.clone();
    assert!(child.deferred_for_scan("source").is_some());
    assert!(child.deferred_for_scan("source").is_none());
    assert!(scope.deferred_reference("source").is_some());
}

#[test]
fn deferred_definitions_keep_only_their_original_lexical_inputs() {
    use std::sync::Arc;

    let mut scope = CteScope::<()>::new();
    let outer = query("WITH q AS (SELECT 7 AS id), unused AS (SELECT 11 AS id) SELECT * FROM q");
    for cte in outer.ctes {
        scope.insert_deferred(cte);
    }
    let original = Arc::clone(&scope.deferred_ctes["q"]);
    let inner = query("WITH r AS (SELECT id FROM q), q AS (SELECT 9 AS id) SELECT * FROM r, q");
    for cte in inner.ctes {
        scope.insert_deferred(cte);
    }
    let definition = scope.definition_scope("r").unwrap();
    assert!(Arc::ptr_eq(&definition.deferred_ctes["q"], &original));
    assert_eq!(definition.deferred_ctes.len(), 1);
    assert!(!Arc::ptr_eq(&scope.deferred_ctes["q"], &original));
    assert!(scope
        .definition_scope("q")
        .unwrap()
        .deferred_ctes
        .is_empty());

    let parent = Arc::clone(&scope.deferred_ctes["q"]);
    let mut same_name = query("WITH q AS (SELECT id FROM q) SELECT * FROM q");
    scope.insert_deferred(same_name.ctes.remove(0));
    let definition = scope.definition_scope("q").unwrap();
    assert!(Arc::ptr_eq(&definition.deferred_ctes["q"], &parent));
    assert!(!uqa_sql::semantics::cte_references_own_name(
        scope.deferred_reference("q").unwrap()
    ));
}

#[test]
fn restored_definition_keeps_shared_inputs_and_invoking_scalar_arena() {
    use super::bindings::{restore_cte_names, save_cte_names};
    use std::{collections::BTreeSet, sync::Arc};

    let mut scope = CteScope::<()>::new();
    let definition = query(
        "WITH q AS (SELECT 7 AS id), r AS NOT MATERIALIZED (SELECT id FROM q) SELECT * FROM r",
    );
    for cte in definition.ctes {
        scope.insert_deferred(cte);
    }
    let original = Arc::clone(&scope.deferred_ctes["q"]);
    let saved = save_cte_names(&scope, &BTreeSet::from(["r".into()]));
    let mut replacement = query("WITH r AS (SELECT 9 AS id) SELECT * FROM r");
    scope.insert_deferred(replacement.ctes.remove(0));
    restore_cte_names(&mut scope, saved);
    scope.scalar_subqueries = vec![query("SELECT 11")].into();
    for _ in 0..3 {
        let (_, input) = scope.take_deferred_input("r").unwrap();
        assert!(Arc::ptr_eq(&input.deferred_ctes["q"], &original));
        assert!(Arc::ptr_eq(
            &input.scalar_subqueries,
            &scope.scalar_subqueries
        ));
    }
}

#[test]
fn not_materialized_definition_survives_repeated_scans_and_shadow_restoration() {
    use super::bindings::{restore_cte_names, save_and_remove_cte_names};
    use std::collections::BTreeSet;

    let mut definition =
        query("WITH source AS NOT MATERIALIZED (SELECT 1 AS value) SELECT value FROM source");
    let mut scope = CteScope::<()>::new();
    scope.insert_deferred(definition.ctes.remove(0));
    for _ in 0..3 {
        assert!(scope.deferred_for_scan("source").is_some());
    }
    let saved = save_and_remove_cte_names(&mut scope, &BTreeSet::from(["source".into()]));
    assert!(scope.deferred_reference("source").is_none());
    restore_cte_names(&mut scope, saved);
    assert!(scope.deferred_reference("source").is_some());
    assert!(scope.deferred_for_scan("source").is_some());
    assert!(scope.deferred_for_scan("source").is_some());
}

#[test]
fn shared_scalar_plans_preserve_arena_cache_separation_and_restoration() {
    use super::subqueries::ScalarSubqueryCacheEntry;
    use uqa_core::Value;

    let mut scope = CteScope::<()>::new();
    scope.scalar_subqueries = vec![query("SELECT 7")].into();
    scope.cache_subquery(0, ScalarSubqueryCacheEntry::Scalar(Value::Int(7)));
    let mut child = scope.clone();
    assert!(matches!(
        child.cached_subquery(0),
        Some(ScalarSubqueryCacheEntry::Scalar(Value::Int(7)))
    ));
    {
        let nested = child.enter_scalar_subqueries(&[query("SELECT 9")]);
        assert!(nested.cached_subquery(0).is_none());
        nested.cache_subquery(0, ScalarSubqueryCacheEntry::Scalar(Value::Int(9)));
        assert!(matches!(
            scope.cached_subquery(0),
            Some(ScalarSubqueryCacheEntry::Scalar(Value::Int(7)))
        ));
    }
    assert!(matches!(
        child.cached_subquery(0),
        Some(ScalarSubqueryCacheEntry::Scalar(Value::Int(7)))
    ));
    let sibling = scope.enter_scalar_subqueries(&[query("SELECT 11")]);
    assert!(sibling.cached_subquery(0).is_none());
}
