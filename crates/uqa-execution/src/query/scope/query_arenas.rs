//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{subqueries::ScalarSubqueryCacheEntry, CteScope};
use uqa_core::{
    memory::{MemoryBudget, ProductionControl},
    CancellationToken, Value,
};
use uqa_sql::{
    plan::{QueryPlan, RelationalPlan, RetainedQueryArena, UnifiedPlan},
    ScalarExpr,
};

fn query(sql: &str) -> QueryPlan {
    let UnifiedPlan::Query(query) = UnifiedPlan::lower(uqa_sql::compile(sql).unwrap().remove(0))
    else {
        panic!("query")
    };
    *query
}

fn row(value: i64) -> crate::OwnedPhysicalRow {
    crate::OwnedPhysicalRow::new(
        crate::RowSchema::new(vec!["n".into()]),
        crate::PhysicalRow::from_values(vec![Value::Int(value)]),
    )
}

#[test]
fn scalar_arena_entry_preserves_parent_state_on_admission_failure_and_both_cancellations() {
    let budget = MemoryBudget::new(16 << 20);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    let mut scope = CteScope::<()>::new();
    scope.scalar_subqueries = RetainedQueryArena::capture(&[query("SELECT 7")], &control).unwrap();
    scope.set_row_lock_outer_row(row(7));
    scope.lock_identities.emit = true;
    scope.lock_identities.retain_after_lock = true;
    scope.cache_subquery(0, ScalarSubqueryCacheEntry::Scalar(Value::Int(7)));
    let parent = scope.scalar_subqueries.clone();
    let identity = scope.scalar_subquery_arena;
    let baseline = budget.used();
    let mut query = query("SELECT (SELECT 9)");
    let RelationalPlan::QueryBlock(block) = &mut query.root else {
        panic!("block")
    };
    let RelationalPlan::QueryBlock(child) = &mut block.subqueries[0].root else {
        panic!("child")
    };
    child.projections[0].expr = ScalarExpr::Literal(Value::Str("x".repeat(4 << 20)));
    for block_entry in [false, true] {
        for available in [0, 64, 1024, 16 << 10] {
            let occupied = budget.reserve(budget.available() - available).unwrap();
            let observed = allocation_counter::measure(|| {
                let result = if block_entry {
                    scope.enter_query_block_with_control(block, &control)
                } else {
                    scope.enter_scalar_subqueries_with_control(&block.subqueries, &control)
                };
                assert!(matches!(result, Err(error) if error.sqlstate() == Some("53200")));
            });
            assert!(observed.bytes_total < 64 << 10, "{observed:?}");
            assert!(scope.scalar_subqueries.ptr_eq(&parent));
            assert_eq!(scope.scalar_subquery_arena, identity);
            assert_eq!(scope.row_lock_outer_row(), Some(&row(7)));
            assert!(scope.lock_identities.emit && scope.lock_identities.retain_after_lock);
            assert!(matches!(
                scope.cached_subquery(0),
                Some(ScalarSubqueryCacheEntry::Scalar(Value::Int(7)))
            ));
            assert_eq!(budget.used(), baseline + occupied.bytes());
            drop(occupied);
        }
        for token in [&original, &invoking] {
            token.cancel();
            let result = if block_entry {
                scope.enter_query_block_with_control(block, &control)
            } else {
                scope.enter_scalar_subqueries_with_control(&block.subqueries, &control)
            };
            assert_eq!(result.err().unwrap().sqlstate(), Some("57014"));
            assert!(scope.scalar_subqueries.ptr_eq(&parent));
            assert_eq!(scope.scalar_subquery_arena, identity);
            assert_eq!(budget.used(), baseline);
            token.reset();
        }
    }
    drop(
        scope
            .enter_query_block_with_control(block, &control)
            .unwrap(),
    );
    assert!(scope.scalar_subqueries.ptr_eq(&parent));
    assert_eq!(budget.used(), baseline);
    drop(scope);
    drop(parent);
    assert_eq!(budget.used(), 0);
}

#[test]
fn admitted_scalar_scopes_keep_child_ownership_after_guard_completion_and_unwind() {
    let budget = MemoryBudget::new(16 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let mut scope = CteScope::<()>::new();
    scope.scalar_subqueries = RetainedQueryArena::capture(&[query("SELECT 7")], &control).unwrap();
    scope.set_row_lock_outer_row(row(7));
    scope.lock_identities.emit = true;
    scope.cache_subquery(0, ScalarSubqueryCacheEntry::Scalar(Value::Int(7)));
    let parent = scope.scalar_subqueries.clone();
    let baseline = budget.used();
    for unwind in [false, true] {
        let mut retained = None;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut child = scope
                .enter_scalar_subqueries_with_control(&[query("SELECT 9")], &control)
                .unwrap();
            assert!(child.cached_subquery(0).is_none());
            child.cache_subquery(0, ScalarSubqueryCacheEntry::Scalar(Value::Int(9)));
            child.set_row_lock_outer_row(row(9));
            child.lock_identities.emit = false;
            retained = Some((*child).clone());
            assert!(!unwind, "child scope unwind");
        }));
        assert_eq!(result.is_err(), unwind);
        assert!(scope.scalar_subqueries.ptr_eq(&parent));
        assert!(scope.lock_identities.emit);
        assert_eq!(scope.row_lock_outer_row(), Some(&row(7)));
        assert!(matches!(
            scope.cached_subquery(0),
            Some(ScalarSubqueryCacheEntry::Scalar(Value::Int(7)))
        ));
        let retained = retained.unwrap();
        assert_eq!(
            budget.used(),
            baseline + retained.scalar_subqueries.reserved_bytes()
        );
        assert_eq!(retained.row_lock_outer_row(), Some(&row(9)));
        assert!(matches!(
            retained.cached_subquery(0),
            Some(ScalarSubqueryCacheEntry::Scalar(Value::Int(9)))
        ));
        drop(retained);
        assert_eq!(budget.used(), baseline);
    }
    drop(parent);
    drop(scope);
    assert_eq!(budget.used(), 0);
}
