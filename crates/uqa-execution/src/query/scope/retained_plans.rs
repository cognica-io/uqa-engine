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
    scope.insert_deferred(definition.ctes.remove(0).into());
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

    let child = scope.clone();
    assert!(child.deferred_for_scan("source").is_some());
    assert!(child.deferred_for_scan("source").is_some());
    assert!(scope.deferred_reference("source").is_some());
}

#[test]
fn deferred_definitions_keep_only_their_original_lexical_inputs() {
    let mut scope = CteScope::<()>::new();
    let outer = query("WITH q AS (SELECT 7 AS id), unused AS (SELECT 11 AS id) SELECT * FROM q");
    for cte in outer.ctes {
        scope.insert_deferred(cte.into());
    }
    let original = scope.deferred_ctes["q"].clone();
    let inner = query("WITH r AS (SELECT id FROM q), q AS (SELECT 9 AS id) SELECT * FROM r, q");
    for cte in inner.ctes {
        scope.insert_deferred(cte.into());
    }
    let definition = scope.definition_scope("r").unwrap();
    assert!(std::ptr::eq(
        definition.deferred_ctes["q"].as_ref(),
        original.as_ref()
    ));
    assert_eq!(definition.deferred_ctes.len(), 1);
    assert!(!std::ptr::eq(
        scope.deferred_ctes["q"].as_ref(),
        original.as_ref()
    ));
    assert!(scope
        .definition_scope("q")
        .unwrap()
        .deferred_ctes
        .is_empty());

    let parent = scope.deferred_ctes["q"].clone();
    let mut same_name = query("WITH q AS (SELECT id FROM q) SELECT * FROM q");
    scope.insert_deferred(same_name.ctes.remove(0).into());
    let definition = scope.definition_scope("q").unwrap();
    assert!(std::ptr::eq(
        definition.deferred_ctes["q"].as_ref(),
        parent.as_ref()
    ));
    assert!(!uqa_sql::semantics::cte_references_own_name(
        scope.deferred_reference("q").unwrap()
    ));
}

#[test]
fn restored_definition_keeps_shared_inputs_and_invoking_scalar_arena() {
    use super::bindings::{restore_cte_names, save_cte_names};
    use std::collections::BTreeSet;

    let mut scope = CteScope::<()>::new();
    let definition = query(
        "WITH q AS (SELECT 7 AS id), r AS NOT MATERIALIZED (SELECT id FROM q) SELECT * FROM r",
    );
    for cte in definition.ctes {
        scope.insert_deferred(cte.into());
    }
    let original = scope.deferred_ctes["q"].clone();
    let saved = save_cte_names(&scope, &BTreeSet::from(["r".into()]));
    let mut replacement = query("WITH r AS (SELECT 9 AS id) SELECT * FROM r");
    scope.insert_deferred(replacement.ctes.remove(0).into());
    restore_cte_names(&mut scope, saved);
    scope.scalar_subqueries = vec![query("SELECT 11")].into();
    for _ in 0..3 {
        let (_, input) = scope.take_deferred_input("r").unwrap();
        assert!(std::ptr::eq(
            input.deferred_ctes["q"].as_ref(),
            original.as_ref()
        ));
        assert!(input.scalar_subqueries.ptr_eq(&scope.scalar_subqueries));
    }
}

#[test]
fn not_materialized_definition_survives_repeated_scans_and_shadow_restoration() {
    use super::bindings::{restore_cte_names, save_and_remove_cte_names};
    use std::collections::BTreeSet;

    let mut definition =
        query("WITH source AS NOT MATERIALIZED (SELECT 1 AS value) SELECT value FROM source");
    let mut scope = CteScope::<()>::new();
    scope.insert_deferred(definition.ctes.remove(0).into());
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

#[test]
fn definition_declarations_do_not_reparse_quoted_names_as_references() {
    for (name, reference) in [("Q.CTE", "\"Q.CTE\""), ("Q.\"CTE", "\"Q.\"\"CTE\"")] {
        let mut definition = query(&format!(
            "WITH {reference} AS MATERIALIZED (SELECT 1 AS value) SELECT * FROM {reference}"
        ));
        assert_eq!(definition.ctes[0].name, name);
        let mut scope = CteScope::<()>::new();
        scope.insert_preparation_cte(definition.ctes.remove(0).into(), false);
        assert!(scope.definition_scope_for_name(name).is_some());
        assert!(scope.definition_scope(reference).is_some());
        assert!(scope.definition_scope(name).is_none());
        assert!(scope.definition_scope("public.\"Q.CTE\"").is_none());
    }
}

#[test]
fn scalar_query_scopes_restore_outer_rows_after_completion_and_unwind() {
    let row = |value| {
        crate::OwnedPhysicalRow::new(
            crate::RowSchema::new(vec!["n".into()]),
            crate::PhysicalRow::from_values(vec![uqa_core::Value::Int(value)]),
        )
    };
    for parent in [None, Some(row(7))] {
        for unwind in [false, true] {
            let mut scope = CteScope::<()>::new();
            if let Some(parent) = &parent {
                scope.set_row_lock_outer_row(parent.clone());
            }
            let mut retained = None;
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let mut nested = scope.enter_scalar_subqueries(&[]);
                nested.set_row_lock_outer_row(row(9));
                retained = Some((*nested).clone());
                assert!(!unwind, "injected query unwind");
            }));
            assert_eq!(result.is_err(), unwind);
            assert_eq!(scope.row_lock_outer_row(), parent.as_ref());
            assert_eq!(retained.unwrap().row_lock_outer_row(), Some(&row(9)));
        }
    }
}

#[test]
fn admitted_cte_payloads_follow_the_last_scope_and_reject_replacement_atomically() {
    use uqa_core::{
        memory::{MemoryBudget, ProductionControl},
        CancellationToken, Value,
    };
    use uqa_sql::plan::{RelationalPlan, RetainedCtePlan};
    let mut statement = query("WITH source AS (SELECT 'value' AS value) SELECT * FROM source");
    let definition = &mut statement.ctes[0];
    let RelationalPlan::QueryBlock(block) = &mut definition.body.query_mut().unwrap().root else {
        panic!("block")
    };
    block.projections[0].expr =
        uqa_sql::ScalarExpr::Literal(Value::Str("x".repeat(4 * 1024 * 1024)));
    let budget = MemoryBudget::new(8 * 1024 * 1024);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    let captured = RetainedCtePlan::capture(definition, &control).unwrap();
    let reserved = budget.used();
    assert!(reserved > 4 * 1024 * 1024);
    let mut scope = CteScope::<()>::new();
    scope.insert_deferred(captured);
    let pointer = std::ptr::from_ref(scope.deferred_reference("source").unwrap());
    let child = scope.clone();
    assert_eq!(budget.used(), reserved);
    let allocation = allocation_counter::measure(|| {
        let rejected = RetainedCtePlan::capture(definition, &control);
        assert!(rejected.is_err());
    });
    assert!(allocation.bytes_total < 64 * 1024, "{allocation:?}");
    assert_eq!(budget.used(), reserved);
    assert_eq!(
        std::ptr::from_ref(scope.deferred_reference("source").unwrap()),
        pointer
    );
    let kept = scope.remove_deferred("source").unwrap();
    assert!(scope.deferred_reference("source").is_none());
    drop(scope);
    drop(kept);
    assert_eq!(budget.used(), reserved);
    assert_eq!(
        std::ptr::from_ref(child.deferred_reference("source").unwrap()),
        pointer
    );
    drop(child);
    assert_eq!(budget.used(), 0);
}

#[test]
fn rejected_cte_shared_wrapper_releases_the_completed_plan_copy() {
    use uqa_core::{
        memory::{MemoryBudget, ProductionControl},
        CancellationToken,
    };
    use uqa_sql::plan::RetainedCtePlan;
    let definition = query("WITH source AS (VALUES (1), (2)) SELECT * FROM source")
        .ctes
        .remove(0);
    let token = CancellationToken::new();
    let measured = MemoryBudget::new(64 * 1024);
    let copy = definition
        .clone_with_control(&ProductionControl::new(&measured, &token, &token))
        .unwrap();
    let payload_bytes = copy.reserved_bytes();
    drop(copy);
    assert_eq!(measured.used(), 0);
    let budget = MemoryBudget::new(payload_bytes);
    assert!(RetainedCtePlan::capture(
        &definition,
        &ProductionControl::new(&budget, &token, &token)
    )
    .is_err());
    assert_eq!(budget.used(), 0);
    for cancel_original in [false, true] {
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if cancel_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        assert!(RetainedCtePlan::capture(
            &definition,
            &ProductionControl::new(&measured, &original, &invoking)
        )
        .is_err());
        assert_eq!(measured.used(), 0);
    }
}
