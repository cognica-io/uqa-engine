//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn primary_query_capture_excludes_installed_declarations_before_copying() {
    let UnifiedPlan::Query(mut input) = one("WITH q AS (SELECT 1) SELECT 2 AS n") else {
        panic!("query");
    };
    input.ctes[0].body = crate::plan::CtePlanBody::Query(Box::new(payload_query(4 << 20)));
    let budget = MemoryBudget::new(64 << 10);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    let allocation = allocation_counter::measure(|| {
        let output = input.copy_primary_with_control(&control).unwrap();
        assert!(output.ctes.is_empty());
        assert!(output.reserved_bytes() > 0);
        assert_eq!(output.relations_bound, input.relations_bound);
        assert_eq!(
            serde_json::to_value(&output.root).unwrap(),
            serde_json::to_value(&input.root).unwrap()
        );
    });
    assert!(allocation.bytes_total < 64 << 10, "{allocation:?}");
    assert_eq!(budget.used(), 0);
    for token in [&original, &invoking] {
        token.cancel();
        assert_eq!(
            input
                .copy_primary_with_control(&control)
                .err()
                .unwrap()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(budget.used(), 0);
        token.reset();
    }
}

#[test]
fn set_ordering_capture_keeps_scalar_slots_and_never_copies_operands() {
    let UnifiedPlan::Query(mut input) =
        one("SELECT 1 AS n UNION ALL SELECT 2 ORDER BY n LIMIT 2 OFFSET 1")
    else {
        panic!("query");
    };
    let RelationalPlan::SetOp {
        left,
        right,
        subqueries,
        ..
    } = &mut input.root
    else {
        panic!("set");
    };
    **left = payload_query(4 << 20);
    **right = payload_query(4 << 20);
    subqueries.push(payload_query(128));
    let budget = MemoryBudget::new(64 << 10);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let allocation = allocation_counter::measure(|| {
        let output = input.set_ordering_with_control(&control).unwrap();
        assert_eq!(output.order_by.len(), 1);
        assert_eq!(output.subqueries.len(), 1);
        let RelationalPlan::SetOp {
            order_by,
            limit,
            offset,
            subqueries,
            ..
        } = &input.root
        else {
            unreachable!();
        };
        assert_eq!(
            serde_json::to_value(&output.order_by).unwrap(),
            serde_json::to_value(order_by).unwrap()
        );
        assert_eq!(
            serde_json::to_value(&output.limit).unwrap(),
            serde_json::to_value(limit).unwrap()
        );
        assert_eq!(
            serde_json::to_value(&output.offset).unwrap(),
            serde_json::to_value(offset).unwrap()
        );
        assert_eq!(
            serde_json::to_value(&output.subqueries).unwrap(),
            serde_json::to_value(subqueries).unwrap()
        );
        assert!(output.reserved_bytes() > 0);
    });
    assert!(allocation.bytes_total < 64 << 10, "{allocation:?}");
    assert_eq!(budget.used(), 0);
    let RelationalPlan::SetOp { order_by, .. } = &mut input.root else {
        unreachable!();
    };
    order_by[0].expr = ScalarExpr::Literal(Value::Str("x".repeat(4 << 20)));
    let allocation = allocation_counter::measure(|| {
        assert_eq!(
            input
                .set_ordering_with_control(&control)
                .err()
                .unwrap()
                .sqlstate(),
            Some("53200")
        );
    });
    assert!(allocation.bytes_total < 16 << 10, "{allocation:?}");
    assert_eq!(budget.used(), 0);
}
