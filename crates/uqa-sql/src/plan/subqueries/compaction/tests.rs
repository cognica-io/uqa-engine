//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::plan::UnifiedPlan;
use uqa_core::{memory::MemoryBudget, CancellationToken, Value};

fn one(sql: &str) -> UnifiedPlan {
    UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0))
}

fn query(sql: &str) -> QueryPlan {
    let UnifiedPlan::Query(query) = one(sql) else {
        panic!("query")
    };
    *query
}

fn expression(count: usize) -> ExpressionPlan {
    ExpressionPlan {
        scalar: ScalarExpr::Row((0..count).map(ScalarExpr::ScalarSubquery).collect()),
        subqueries: (0..count)
            .map(|index| query(&format!("SELECT {index}")))
            .collect(),
    }
}

#[test]
fn compaction_admits_scratch_before_mutation_and_releases_partial_failure() {
    let mut plan = expression(1024);
    let before = serde_json::to_value(&plan).unwrap();
    let parent = MemoryBudget::new(8192);
    let sibling = parent.reserve(512).unwrap();
    let token = CancellationToken::new();
    for allowance in [0, 1, 1024] {
        let memory = parent.child(allowance);
        let mut result = None;
        let allocation = allocation_counter::measure(|| {
            result = Some(prune_expression_with_control(
                &mut plan,
                ProductionControl::new(&memory, &token, &token),
            ));
        });
        assert_eq!(result.unwrap().unwrap_err().sqlstate(), Some("53200"));
        assert!(allocation.bytes_total < 8192, "{allocation:?}");
        assert_eq!(memory.used(), 0);
        assert_eq!(parent.used(), sibling.bytes());
        assert_eq!(serde_json::to_value(&plan).unwrap(), before);
    }
}

#[test]
fn compaction_moves_surviving_queries_without_reallocating_the_arena() {
    let mut plan = expression(8);
    plan.scalar = ScalarExpr::Row([6, 2, 6, 7].map(ScalarExpr::ScalarSubquery).into());
    let children: Vec<_> = [2, 6, 7]
        .into_iter()
        .map(|id| serde_json::to_value(&plan.subqueries[id]).unwrap())
        .collect();
    let buffer = plan.subqueries.as_ptr();
    let capacity = plan.subqueries.capacity();
    let parent = MemoryBudget::new(8192);
    let sibling = parent.reserve(512).unwrap();
    let memory = parent.child(4096);
    let token = CancellationToken::new();
    let allocation = allocation_counter::measure(|| {
        prune_expression_with_control(&mut plan, ProductionControl::new(&memory, &token, &token))
            .unwrap();
    });
    assert_eq!(plan.subqueries.as_ptr(), buffer);
    assert_eq!(plan.subqueries.capacity(), capacity);
    assert_eq!(
        plan.scalar,
        ScalarExpr::Row([1, 0, 1, 2].map(ScalarExpr::ScalarSubquery).into())
    );
    assert_eq!(
        serde_json::to_value(&plan.subqueries).unwrap(),
        serde_json::to_value(children).unwrap()
    );
    assert_eq!(
        allocation.bytes_total,
        memory.peak() as u64,
        "only admitted map nodes allocate"
    );
    assert!(memory.peak() > 0);
    assert_eq!(memory.used(), 0);
    assert_eq!(parent.used(), sibling.bytes());
}

#[test]
fn compaction_preserves_mutation_sources_and_independent_query_namespaces() {
    let token = CancellationToken::new();
    let memory = MemoryBudget::new(8192);
    let control = ProductionControl::new(&memory, &token, &token);
    for sql in [
        "INSERT INTO t(a) VALUES ((SELECT 1)) ON CONFLICT (a) DO UPDATE SET a = (SELECT 2) RETURNING (SELECT 3)",
        "UPDATE t SET a = (SELECT 1) FROM LATERAL generate_series(1, (SELECT 2)) g WHERE a = (SELECT 3) RETURNING (SELECT 4)",
        "DELETE FROM t USING (VALUES ((SELECT 1))) v(a) WHERE t.a = (SELECT 2) RETURNING (SELECT 3)",
        "MERGE INTO t USING (SELECT (SELECT 10) AS a) u ON t.a = (SELECT 1) WHEN MATCHED THEN UPDATE SET a = (SELECT 2) WHEN NOT MATCHED THEN INSERT (a) VALUES ((SELECT 3)) RETURNING (SELECT 4)",
    ] {
        let UnifiedPlan::Command(mut command) = one(sql) else { panic!("command") };
        let before = serde_json::to_value(&command).unwrap();
        command_arena(&mut command).unwrap().push(query("SELECT 999"));
        prune_command_with_control(&mut command, control).unwrap();
        assert_eq!(serde_json::to_value(&command).unwrap(), before, "{sql}");
        assert_eq!(memory.used(), 0);
    }
    for sql in [
        "VALUES ((SELECT 1)), ((SELECT 2))",
        "SELECT (SELECT 1) UNION ALL SELECT (SELECT 2) LIMIT (SELECT 3)",
        "SELECT (SELECT 1) FROM (SELECT (SELECT 10)) q",
    ] {
        let mut query = query(sql);
        let before = serde_json::to_value(&query).unwrap();
        query_arena(&mut query.root).push(self::query("SELECT 999"));
        prune_query_with_control(&mut query, control).unwrap();
        assert_eq!(serde_json::to_value(&query).unwrap(), before, "{sql}");
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn compaction_retains_canonical_window_inputs_after_discarding_a_projection() {
    let mut query = query("SELECT (SELECT 10), sum(1) OVER (PARTITION BY (SELECT 20) ORDER BY (SELECT 30) ROWS BETWEEN (SELECT 1) PRECEDING AND CURRENT ROW)");
    let RelationalPlan::QueryBlock(block) = &mut query.root else {
        panic!("block")
    };
    assert_eq!(block.subqueries.len(), 4);
    block.projections.remove(0);
    let expected: Vec<_> = block.subqueries[1..]
        .iter()
        .map(|q| serde_json::to_value(q).unwrap())
        .collect();
    let token = CancellationToken::new();
    let memory = MemoryBudget::new(8192);
    prune_query_with_control(&mut query, ProductionControl::new(&memory, &token, &token)).unwrap();
    let RelationalPlan::QueryBlock(block) = &query.root else {
        panic!("block")
    };
    assert_eq!(
        serde_json::to_value(&block.subqueries).unwrap(),
        serde_json::to_value(expected).unwrap()
    );
    let mut ids = std::collections::BTreeSet::new();
    for window in &block.windows {
        for expression in window.spec.expressions() {
            expression.visit(&mut |node| {
                if let ScalarExpr::ScalarSubquery(id) = node {
                    ids.insert(*id);
                }
            });
        }
    }
    assert_eq!(ids.into_iter().collect::<Vec<_>>(), [0, 1, 2]);
    assert_eq!(memory.used(), 0);
}

#[test]
fn compaction_preserves_invalid_references_and_zero_allocation_empty_cases() {
    let token = CancellationToken::new();
    let memory = MemoryBudget::new(8192);
    let control = ProductionControl::new(&memory, &token, &token);
    let mut plan = expression(3);
    plan.scalar = ScalarExpr::Row(vec![
        ScalarExpr::ScalarSubquery(2),
        ScalarExpr::ScalarSubquery(usize::MAX),
    ]);
    let before = serde_json::to_value(&plan).unwrap();
    prune_expression_with_control(&mut plan, control).unwrap();
    assert_eq!(serde_json::to_value(&plan).unwrap(), before);
    plan.scalar = ScalarExpr::Literal(Value::Int(1));
    let allocation = allocation_counter::measure(|| {
        prune_expression_with_control(&mut plan, control).unwrap();
        prune_expression_with_control(&mut plan, control).unwrap();
    });
    assert_eq!(allocation.bytes_total, 0);
    assert!(plan.subqueries.is_empty());
    assert_eq!(memory.used(), 0);
}

#[test]
fn compaction_observes_both_cancellations_before_changing_query_ids() {
    let mut plan = expression(8);
    let before = serde_json::to_value(&plan).unwrap();
    for cancel_original in [false, true] {
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let memory = MemoryBudget::new(0);
        if cancel_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        let error = prune_expression_with_control(
            &mut plan,
            ProductionControl::new(&memory, &original, &invoking),
        )
        .unwrap_err();
        assert_eq!(error.sqlstate(), Some("57014"));
        assert_eq!(memory.peak(), 0);
        assert_eq!(serde_json::to_value(&plan).unwrap(), before);
    }
}

#[test]
fn compaction_scratch_retires_after_cancellation_and_unwind() {
    let parent = MemoryBudget::new(8192);
    let sibling = parent.reserve(512).unwrap();
    for cancel_original in [false, true] {
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let memory = parent.child(4096);
        let mut remap = Remapping::new(8, ProductionControl::new(&memory, &original, &invoking));
        remap.expression(&ScalarExpr::ScalarSubquery(5)).unwrap();
        assert!(memory.used() > 0);
        if cancel_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        assert_eq!(remap.prepare().unwrap_err().sqlstate(), Some("57014"));
        drop(remap);
        assert_eq!(parent.used(), sibling.bytes());
    }
    let token = CancellationToken::new();
    let memory = parent.child(4096);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut remap = Remapping::new(8, ProductionControl::new(&memory, &token, &token));
        remap.expression(&ScalarExpr::ScalarSubquery(5)).unwrap();
        panic!("producer unwind");
    }));
    assert!(result.is_err());
    assert_eq!(parent.used(), sibling.bytes());
}

#[test]
fn fallible_query_traversal_stops_before_later_query_owners() {
    for sql in [
        "WITH q AS (SELECT 1) SELECT (SELECT 2) FROM (SELECT 3) t UNION ALL SELECT 4",
        "WITH q AS (DELETE FROM t RETURNING (SELECT 1)) INSERT INTO t SELECT (SELECT 2) RETURNING (SELECT 3)",
        "EXPLAIN SELECT (SELECT 1), (SELECT 2)",
        "CALL p((SELECT 1), (SELECT 2))",
    ] {
        let mut plan = one(sql);
        let mut identities = Vec::new();
        plan.visit_queries_mut(&mut |query| identities.push(std::ptr::from_mut(query)));
        for stop in 0..identities.len() {
            let mut visited = Vec::new();
            let result = plan.try_visit_queries_mut(&mut |query| {
                visited.push(std::ptr::from_mut(query));
                if visited.len() == stop + 1 { Err(stop) } else { Ok(()) }
            });
            assert_eq!(result, Err(stop));
            assert_eq!(visited, identities[..=stop], "{sql}");
        }
    }
}

#[test]
fn compaction_releases_discarded_constant_owners_without_detaching_survivors() {
    use crate::ir::{RetainedConstantValue, RetainedScalarLiteral};
    let parent = MemoryBudget::new(16384);
    let sibling = parent.reserve(512).unwrap();
    let constants = parent.child(8192);
    let scratch = parent.child(4096);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&constants, &token, &token);
    let mut plan = expression(2);
    let mut first_bytes = 0;
    for (index, query) in plan.subqueries.iter_mut().enumerate() {
        let literal = RetainedScalarLiteral::with_control(
            RetainedConstantValue::from_produced(
                control.copy_value(&Value::Str("x".repeat(256))).unwrap(),
                &control,
            )
            .unwrap(),
            None,
            None,
            None,
            &control,
        )
        .unwrap();
        let RelationalPlan::QueryBlock(block) = &mut query.root else {
            panic!("block")
        };
        block.projections[0].expr = ScalarExpr::RetainedLiteral(literal);
        if index == 0 {
            first_bytes = constants.used();
        }
    }
    let before = constants.used();
    plan.scalar = ScalarExpr::ScalarSubquery(1);
    prune_expression_with_control(&mut plan, ProductionControl::new(&scratch, &token, &token))
        .unwrap();
    assert_eq!(constants.used(), before - first_bytes);
    assert_eq!(scratch.used(), 0);
    let RelationalPlan::QueryBlock(block) = &plan.subqueries[0].root else {
        panic!("block")
    };
    let ScalarExpr::RetainedLiteral(literal) = &block.projections[0].expr else {
        panic!("retained")
    };
    let survivor = literal.clone();
    drop(plan);
    assert_eq!(constants.used(), before - first_bytes);
    drop(survivor);
    assert_eq!(parent.used(), sibling.bytes());
}
