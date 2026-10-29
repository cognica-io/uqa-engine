//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};
use uqa_sql::plan::{QueryPlan, RelationalPlan, UnifiedPlan};

fn query(sql: &str) -> QueryPlan {
    let UnifiedPlan::Query(query) = UnifiedPlan::lower(uqa_sql::compile(sql).unwrap().remove(0))
    else {
        panic!("query")
    };
    *query
}

#[test]
fn live_subquery_scratch_rejects_before_unbounded_collection() {
    let expression = ScalarExpr::Row((0..1024).map(ScalarExpr::ScalarSubquery).collect());
    let parent = MemoryBudget::new(8192);
    let sibling = parent.reserve(512).unwrap();
    let token = CancellationToken::new();
    for allowance in [0, 1, 1024] {
        let memory = parent.child(allowance);
        let mut result = None;
        let allocation = allocation_counter::measure(|| {
            result = Some(live_slots(
                [&expression],
                ProductionControl::new(&memory, &token, &token),
            ));
        });
        let Err(error) = result.unwrap() else {
            panic!("admission must fail")
        };
        assert_eq!(error.sqlstate(), Some("53200"));
        assert!(allocation.bytes_total < 8192, "{allocation:?}");
        assert_eq!(memory.used(), 0);
        assert_eq!(parent.used(), sibling.bytes());
    }
}

#[test]
fn live_subquery_scratch_owns_distinct_nodes_until_its_last_reader_finishes() {
    let expression = ScalarExpr::Row([5, 2, 5, 1].map(ScalarExpr::ScalarSubquery).into());
    let parent = MemoryBudget::new(8192);
    let sibling = parent.reserve(512).unwrap();
    let memory = parent.child(4096);
    let token = CancellationToken::new();
    let mut slots = None;
    let allocation = allocation_counter::measure(|| {
        slots = Some(
            live_slots(
                [&expression],
                ProductionControl::new(&memory, &token, &token),
            )
            .unwrap(),
        );
    });
    let slots = slots.unwrap();
    assert_eq!(
        slots.iter().map(|(id, ())| *id).collect::<Vec<_>>(),
        [1, 2, 5]
    );
    assert_eq!(allocation.bytes_current, memory.used() as i64);
    assert_eq!(allocation.bytes_total, memory.used() as u64);
    drop(slots);
    assert_eq!(parent.used(), sibling.bytes());
}

#[test]
fn live_subquery_planning_keeps_dead_case_and_coalesce_children_for_typing() {
    let token = CancellationToken::new();
    let memory = MemoryBudget::new(16384);
    let mut config = super::super::OptimizerConfig::new(
        uqa_execution::scalar::eval_constant_scalar_with_control,
    );
    config.production = ProductionControl::new(&memory, &token, &token);
    for sql in [
        "SELECT CASE WHEN false THEN (SELECT 1/0) WHEN true THEN (SELECT 2) ELSE (SELECT 1/0) END",
        "SELECT CASE 2 WHEN 1 THEN (SELECT 1/0) WHEN 2 THEN (SELECT 2) ELSE (SELECT 1/0) END",
        "SELECT COALESCE(NULL, 1, (SELECT 1/0))",
    ] {
        let mut query = query(sql);
        let RelationalPlan::QueryBlock(block) = &mut query.root else {
            panic!("block")
        };
        let dead = serde_json::to_value(&block.subqueries[0]).unwrap();
        let slots = live_slots_for_block(block, config.production).unwrap();
        let expected: &[usize] = if sql.contains("COALESCE") { &[] } else { &[1] };
        assert_eq!(
            slots.iter().map(|(id, ())| *id).collect::<Vec<_>>(),
            expected
        );
        optimize_live(&mut block.subqueries, &slots, &config, &|_: &str| false).unwrap();
        assert_eq!(serde_json::to_value(&block.subqueries[0]).unwrap(), dead);
        drop(slots);
        drop(query);
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn live_subquery_collection_observes_original_and_invoking_cancellation() {
    for cancel_original in [false, true] {
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let memory = MemoryBudget::new(0);
        if cancel_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        let control = ProductionControl::new(&memory, &original, &invoking);
        for expressions in [vec![], vec![ScalarExpr::ScalarSubquery(0)]] {
            let Err(error) = live_slots(&expressions, control) else {
                panic!("cancelled")
            };
            assert_eq!(error.sqlstate(), Some("57014"));
        }
        assert_eq!(memory.peak(), 0);
    }
}
