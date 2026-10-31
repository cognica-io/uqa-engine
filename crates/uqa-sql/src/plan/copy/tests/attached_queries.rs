//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::plan::{OwnedQueryPlan, SourcePlan};

fn query(sql: &str) -> QueryPlan {
    let UnifiedPlan::Query(query) = one(sql) else {
        panic!("query")
    };
    *query
}
fn owned(sql: &str, control: &ProductionControl<'_>) -> OwnedQueryPlan {
    OwnedQueryPlan::from_produced(
        query(sql)
            .copy_for_rewrite(control)
            .unwrap()
            .finish(control)
            .unwrap(),
        control,
    )
    .unwrap()
}

#[test]
fn attached_queries_keep_original_roots_without_copying_replaced_bodies() {
    let source = query(&format!(
        "SELECT * FROM LATERAL (SELECT '{}' AS v) q(v)",
        "x".repeat(4 << 20)
    ));
    let RelationalPlan::QueryBlock(block) = &source.root else {
        panic!("block")
    };
    let source = block.from.as_ref().unwrap();
    let set = query(&format!(
        "SELECT '{}' AS v UNION ALL SELECT '{}' ORDER BY 1 LIMIT 5 OFFSET 1",
        "x".repeat(4 << 20),
        "y".repeat(4 << 20)
    ));
    let budget = MemoryBudget::new(64 << 10);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let mut source_output = None;
    let mut set_output = None;
    let replacement = owned("SELECT 7 AS v", &control);
    let left = owned("SELECT 7 AS v", &control);
    let right = owned("SELECT 9 AS v", &control);
    let baseline = budget.used();
    let body_address = std::ptr::from_ref(replacement.as_ref());
    let left_address = std::ptr::from_ref(left.as_ref());
    let right_address = std::ptr::from_ref(right.as_ref());
    let allocation = allocation_counter::measure(|| {
        source_output = Some(source.copy_with_query(replacement, &control).unwrap());
        let SourcePlan::Subquery {
            body,
            lateral,
            alias,
            column_aliases,
            ..
        } = &**source_output.as_ref().unwrap()
        else {
            panic!("source")
        };
        assert_eq!(std::ptr::from_ref(body.as_ref()), body_address);
        assert!(*lateral);
        assert_eq!(alias.as_deref(), Some("q"));
        assert_eq!(column_aliases, &["v"]);
        set_output = Some(set.copy_with_set_inputs(left, right, &control).unwrap());
        let RelationalPlan::SetOp {
            left,
            right,
            all,
            order_by,
            limit,
            offset,
            ..
        } = &set_output.as_ref().unwrap().root
        else {
            panic!("set")
        };
        assert_eq!(std::ptr::from_ref(left.as_ref()), left_address);
        assert_eq!(std::ptr::from_ref(right.as_ref()), right_address);
        assert!(*all);
        assert_eq!(order_by.len(), 1);
        assert!(limit.is_some() && offset.is_some());
    });
    assert_eq!(allocation.bytes_current, (budget.used() - baseline) as i64);
    assert!(allocation.bytes_total < 256 << 10, "{allocation:?}");
    drop(source_output);
    assert!(budget.used() > 0);
    drop(set_output);
    assert_eq!(budget.used(), 0);
}

#[test]
fn attached_query_declarations_reject_before_copying_and_retire_consumed_inputs() {
    let mut input = query("SELECT * FROM LATERAL (SELECT 1 AS v) q(v)");
    let RelationalPlan::QueryBlock(block) = &mut input.root else {
        panic!("block")
    };
    let source = block.from.as_mut().unwrap();
    let SourcePlan::Subquery { alias, .. } = source else {
        panic!("source")
    };
    *alias = Some("x".repeat(4 << 20));
    let budget = MemoryBudget::new(32 << 10);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    for failure in 0..3 {
        let replacement = owned("SELECT 7 AS v", &control);
        if failure == 1 {
            original.cancel();
        }
        if failure == 2 {
            invoking.cancel();
        }
        let allocation = allocation_counter::measure(|| {
            assert_eq!(
                source
                    .copy_with_query(replacement, &control)
                    .unwrap_err()
                    .sqlstate(),
                Some(if failure == 0 { "53200" } else { "57014" })
            );
        });
        assert!(allocation.bytes_total < 128 << 10, "{allocation:?}");
        assert_eq!(budget.used(), 0);
        original.reset();
        invoking.reset();
    }
}
