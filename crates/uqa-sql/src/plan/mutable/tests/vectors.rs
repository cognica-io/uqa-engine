//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{fixture, MemoryBudget, ProductionControl};
use crate::{
    plan::{OwnedScalarExpr, QueryScalarSlot, RelationalPlan},
    ScalarExpr,
};
use uqa_core::{CancellationToken, Value};

#[test]
fn replacing_group_keys_releases_discarded_payloads_and_keeps_other_owners() {
    let mut source = fixture();
    let RelationalPlan::QueryBlock(block) = &mut source.root else {
        unreachable!()
    };
    block.group_by = vec![ScalarExpr::Literal(Value::Str("discarded".repeat(8192)))];
    let budget = MemoryBudget::new(4 * 1024 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let query = source.copy_for_rewrite(&control).unwrap();
    let value = OwnedScalarExpr::literal(
        control
            .finish(Value::Bool(true), control.empty_reservation())
            .unwrap(),
        &control,
    )
    .unwrap();
    let before = budget.used();
    let mut output = None;
    let changed = allocation_counter::measure(|| {
        output = Some(
            query
                .replace_group_by(std::iter::once(value), &control)
                .unwrap(),
        );
    });
    let query = output.unwrap();
    assert_eq!(before as i64 + changed.bytes_current, budget.used() as i64);
    assert_eq!(query.reserved_bytes(), budget.used());
    let RelationalPlan::QueryBlock(block) = &query.root else {
        unreachable!()
    };
    assert_eq!(block.group_by, [ScalarExpr::Literal(Value::Bool(true))]);
    assert_eq!(block.projections.len(), 2);
    let query = query
        .map_scalar(QueryScalarSlot::GroupBy(0), &control, |_| {
            Ok(OwnedScalarExpr::literal(
                control.finish(Value::Int(1), control.empty_reservation())?,
                &control,
            )?)
        })
        .unwrap();
    let frozen = query.finish(&control).unwrap();
    assert_eq!(budget.used(), frozen.reserved_bytes());
    drop(frozen);
    assert_eq!(budget.used(), 0);
}

#[test]
fn rejected_group_container_drops_consumed_query_and_input_owners() {
    let source = fixture();
    let budget = MemoryBudget::new(4 * 1024 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let query = source.copy_for_rewrite(&control).unwrap();
    let value = OwnedScalarExpr::literal(
        control.copy_value(&Value::Str("new".repeat(1024))).unwrap(),
        &control,
    )
    .unwrap();
    let held = budget.reserve(budget.limit() - budget.used()).unwrap();
    assert_eq!(
        query
            .replace_group_by(std::iter::once(value), &control)
            .unwrap_err()
            .sqlstate(),
        Some("53200")
    );
    assert_eq!(budget.used(), held.bytes());
    drop(held);
    assert_eq!(budget.used(), 0);
}

#[test]
fn interrupted_group_iterator_retires_each_transferred_and_unvisited_child() {
    let source = fixture();
    let budget = MemoryBudget::new(4 * 1024 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let query = source.copy_for_rewrite(&control).unwrap();
    let first = ScalarExpr::Literal(Value::Str("first".repeat(1024)))
        .copy_for_rewrite(&control)
        .unwrap();
    let second = ScalarExpr::Literal(Value::Str("second".repeat(1024)))
        .copy_for_rewrite(&control)
        .unwrap();
    let failure = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let iterator = [first, second]
            .into_iter()
            .enumerate()
            .map(|(index, item)| {
                assert_eq!(index, 0, "injected iterator unwind");
                item
            });
        let _ = query.replace_group_by(iterator, &control);
    }));
    assert!(failure.is_err());
    assert_eq!(budget.used(), 0);
}
