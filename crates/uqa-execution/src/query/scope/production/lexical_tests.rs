//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};
use uqa_sql::plan::{QueryPlan, RetainedCtePlan, UnifiedPlan};

fn query(sql: &str) -> QueryPlan {
    let UnifiedPlan::Query(query) = UnifiedPlan::lower(uqa_sql::compile(sql).unwrap().remove(0))
    else {
        panic!("query");
    };
    *query
}

fn declaration(sql: &str) -> RetainedCtePlan {
    query(sql).ctes.remove(0).into()
}

fn rows() -> crate::SharedSpill {
    crate::SpillBuffer::new(4096)
        .into_shared(crate::RowSchema::new(vec!["n".into()]))
        .unwrap()
}

#[test]
fn lexical_copy_rejects_before_allocating_names_and_keeps_shared_inputs_alive() {
    let mut definition = query("WITH q AS (SELECT 1) SELECT 1").ctes.remove(0);
    definition.name = "x".repeat(4 << 20);
    let mut source = CteScope::<()>::new();
    source.insert_deferred(definition.into());
    let budget = MemoryBudget::new(4096);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    let measured = allocation_counter::measure(|| {
        assert_eq!(
            source.copy_with_control(&control).err().unwrap().sqlstate(),
            Some("53200")
        );
    });
    assert!(measured.bytes_total < 16 << 10, "{measured:?}");
    assert_eq!(budget.used(), 0);
    drop(source);

    let budget = MemoryBudget::new(1 << 20);
    let control = ProductionControl::new(&budget, &original, &invoking);
    let mut source = CteScope::<()>::empty_with_control(&control).unwrap();
    source
        .insert_deferred_with_control(declaration("WITH q AS (SELECT 1) SELECT 1"), &control)
        .unwrap();
    source
        .insert_shared_with_control("rows", rows(), &control)
        .unwrap();
    let original_plan = source.deferred_for_scan("q").unwrap();
    let before = budget.used();
    let mut copy = source.copy_with_control(&control).unwrap();
    assert!(budget.used() > before);
    assert!(std::ptr::eq(
        copy.deferred_reference("q").unwrap(),
        original_plan.as_ref()
    ));
    copy.remove_deferred("q");
    copy.remove_materialized("rows");
    assert!(source.deferred_reference("q").is_some());
    assert!(source.rows.contains_key("rows"));
    for token in [&original, &invoking] {
        token.cancel();
        let held = budget.used();
        assert_eq!(
            source.copy_with_control(&control).err().unwrap().sqlstate(),
            Some("57014")
        );
        assert_eq!(budget.used(), held);
        token.reset();
    }
    drop((source, copy, original_plan));
    assert_eq!(budget.used(), 0);
}

#[test]
fn declaration_replacement_is_atomic_at_each_partial_allocation_boundary() {
    let initial = query("WITH source AS (SELECT 7 AS n), q AS (SELECT 9 AS n) SELECT 1");
    let replacement = declaration("WITH q AS (SELECT n FROM source) SELECT 1");
    let mut failed = 0;
    let mut succeeded = 0;
    for available in [0, 64, 128, 256, 512, 1024, 2048, 4096, 8192, 16384] {
        let budget = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let mut scope = CteScope::<()>::empty_with_control(&control).unwrap();
        for definition in &initial.ctes {
            scope
                .insert_deferred_with_control(definition.clone().into(), &control)
                .unwrap();
        }
        let before = budget.used();
        let original = scope.deferred_for_scan("q").unwrap();
        let original_definition = Arc::clone(scope.definition_scopes.get("q").unwrap());
        let occupied = budget.reserve(budget.available() - available).unwrap();
        match scope.insert_preparation_cte_with_control(replacement.clone(), false, &control) {
            Err(error) => {
                failed += 1;
                assert_eq!(error.sqlstate(), Some("53200"));
                assert!(std::ptr::eq(
                    scope.deferred_reference("q").unwrap(),
                    original.as_ref()
                ));
                assert!(Arc::ptr_eq(
                    scope.definition_scopes.get("q").unwrap(),
                    &original_definition
                ));
                assert!(scope.cte_is_folded("q"));
                assert!(!scope.non_returning_ctes.contains("q"));
                assert_eq!(budget.used(), before + occupied.bytes());
            }
            Ok(()) => {
                succeeded += 1;
                assert!(std::ptr::eq(
                    scope.deferred_reference("q").unwrap(),
                    replacement.as_ref()
                ));
                assert!(!scope.cte_is_folded("q"));
            }
        }
        drop((occupied, scope, original, original_definition));
        assert_eq!(budget.used(), 0);
    }
    assert!(
        failed >= 3 && succeeded >= 1,
        "failed={failed}, succeeded={succeeded}"
    );
}

#[test]
fn lexical_guards_restore_only_selected_names_without_allocation_even_when_cancelled() {
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let mut scope = CteScope::<()>::empty_with_control(&control).unwrap();
    scope
        .insert_deferred_with_control(declaration("WITH q AS (SELECT 7 AS n) SELECT 1"), &control)
        .unwrap();
    let original = scope.deferred_for_scan("q").unwrap();
    let materialized = rows();
    {
        let mut guard = scope
            .enter_cte_bindings_with_control(["q"], &control)
            .unwrap();
        guard
            .insert_shared_with_control("q", materialized.clone(), &control)
            .unwrap();
        guard
            .insert_shared_with_control("progress", materialized.clone(), &control)
            .unwrap();
        let occupied = budget.reserve(budget.available()).unwrap();
        token.cancel();
        let allocation = allocation_counter::measure(|| drop(guard));
        assert_eq!(allocation.count_total, 0, "{allocation:?}");
        drop(occupied);
        token.reset();
    }
    assert!(std::ptr::eq(
        scope.deferred_reference("q").unwrap(),
        original.as_ref()
    ));
    assert!(scope.rows.contains_key("progress"));
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut guard = scope
            .enter_cte_bindings_with_control(["q"], &control)
            .unwrap();
        guard
            .insert_shared_with_control("q", materialized, &control)
            .unwrap();
        panic!("unwind lexical binding");
    }));
    assert!(result.is_err());
    assert!(std::ptr::eq(
        scope.deferred_reference("q").unwrap(),
        original.as_ref()
    ));
    assert!(scope.rows.contains_key("progress"));
    drop((scope, original));
    assert_eq!(budget.used(), 0);
}

#[test]
fn definition_scope_copies_only_selected_names_and_preserves_quoted_identity() {
    let mut scope = CteScope::<()>::new();
    for cte in
        query("WITH source AS (SELECT 7 AS n), selected AS (SELECT n FROM source) SELECT 1").ctes
    {
        scope.insert_deferred(cte.into());
    }
    let mut unused = query("WITH unused AS (SELECT 1) SELECT 1").ctes.remove(0);
    unused.name = "x".repeat(4 << 20);
    scope.insert_deferred(unused.into());
    let budget = MemoryBudget::new(64 << 10);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let measured = allocation_counter::measure(|| {
        let definition = scope
            .definition_scope_with_control("selected", &control)
            .unwrap()
            .unwrap();
        assert_eq!(definition.deferred_ctes.len(), 1);
        assert!(definition.deferred_reference("source").is_some());
    });
    assert!(measured.bytes_total < 64 << 10, "{measured:?}");
    assert_eq!(budget.used(), 0);
    for (decoded, reference) in [
        ("Q.CTE", "\"Q.CTE\""),
        ("a\"b", "\"a\"\"b\""),
        ("한글", "한글"),
    ] {
        let mut plan = query("WITH q AS (SELECT 1) SELECT 1").ctes.remove(0);
        plan.name = decoded.into();
        scope.insert_deferred(plan.into());
        let occupied = budget.reserve(budget.available()).unwrap();
        let measured = allocation_counter::measure(|| {
            assert!(scope
                .deferred_reference_with_control(reference, &control)
                .unwrap()
                .is_some());
            assert!(scope
                .is_visible_cte_with_control(reference, &control)
                .unwrap());
        });
        assert_eq!(measured.count_total, 0, "{measured:?}");
        drop(occupied);
    }
}

#[test]
fn recursive_working_binding_publishes_and_restores_rows_and_width_atomically() {
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let mut scope = CteScope::<()>::empty_with_control(&control).unwrap();
    scope
        .insert_deferred_with_control(declaration("WITH q AS (SELECT 7 AS n) SELECT 1"), &control)
        .unwrap();
    scope
        .recursive_control_widths
        .insert_with_control("q", 7, &control)
        .unwrap();
    let original = scope.deferred_for_scan("q").unwrap();
    let before = budget.used();
    let materialized = rows();
    for available in [0, 64, 128] {
        let occupied = budget.reserve(budget.available() - available).unwrap();
        assert_eq!(
            scope
                .enter_recursive_cte_with_control("q", materialized.clone(), Some(1), &control)
                .err()
                .unwrap()
                .sqlstate(),
            Some("53200")
        );
        assert!(std::ptr::eq(
            scope.deferred_reference("q").unwrap(),
            original.as_ref()
        ));
        assert_eq!(scope.recursive_control_width("q"), Some(7));
        assert_eq!(budget.used(), before + occupied.bytes());
        drop(occupied);
    }
    let guard = scope
        .enter_recursive_cte_with_control("q", materialized.clone(), Some(1), &control)
        .unwrap();
    assert_eq!(guard.recursive_control_width("q"), Some(1));
    assert!(guard.rows.contains_key("q"));
    let occupied = budget.reserve(budget.available()).unwrap();
    token.cancel();
    let allocation = allocation_counter::measure(|| drop(guard));
    assert_eq!(allocation.count_total, 0);
    token.reset();
    drop(occupied);
    assert_eq!(budget.used(), before);
    assert!(std::ptr::eq(
        scope.deferred_reference("q").unwrap(),
        original.as_ref()
    ));
    assert_eq!(scope.recursive_control_width("q"), Some(7));
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _guard = scope
            .enter_recursive_cte_with_control("q", materialized, Some(1), &control)
            .unwrap();
        panic!("unwind recursive binding");
    }));
    assert!(result.is_err());
    assert_eq!(scope.recursive_control_width("q"), Some(7));
    assert_eq!(budget.used(), before);
    drop((scope, original));
    assert_eq!(budget.used(), 0);
}
