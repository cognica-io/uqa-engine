//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::bindings::{PredicateBindings, PredicateGroups};
use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken, Value};
use uqa_sql::plan::OwnedScalarExpr;

#[test]
fn predicate_slots_retain_the_buffer_lease_when_expressions_move() {
    let bytes = 4 * size_of::<Option<OwnedScalarExpr>>();
    let budget = MemoryBudget::new(bytes + 13);
    let occupied = budget.reserve(13).unwrap();
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let payload = "x".repeat(4 * 1024 * 1024);
    let address = payload.as_ptr();
    let mut output = None;
    let allocation = allocation_counter::measure(|| {
        let mut slots = PredicateBindings::new(4, &control).unwrap();
        slots.set(
            0,
            OwnedScalarExpr::uncontrolled(ScalarExpr::Literal(Value::Str(payload))),
        );
        for index in 1..4 {
            slots.set(
                index as u64,
                OwnedScalarExpr::uncontrolled(ScalarExpr::Param(index)),
            );
        }
        output = Some(slots);
    });
    assert_eq!(allocation.bytes_total, u64::try_from(bytes).unwrap());
    assert_eq!(budget.used(), bytes + 13);
    let mut slots = output.unwrap();
    let moved = slots.take(0..1).next().unwrap();
    let ScalarExpr::Literal(Value::Str(payload)) = &*moved else {
        panic!("moved source payload")
    };
    assert_eq!(payload.as_ptr(), address);
    assert_eq!(budget.used(), bytes + 13);
    drop(slots);
    assert_eq!(budget.used(), 13);
    let failure = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _slots = PredicateBindings::new(4, &control).unwrap();
        assert_eq!(budget.used(), bytes + 13);
        panic!("consumer unwind");
    }));
    assert!(failure.is_err());
    assert_eq!(budget.used(), 13);
    let allocation = allocation_counter::measure(|| {
        assert_eq!(
            PredicateBindings::new(65_536, &control)
                .err()
                .expect("slot admission")
                .sqlstate(),
            Some("53200")
        );
    });
    assert!(allocation.bytes_total < 4096, "{allocation:?}");
    assert_eq!(budget.used(), 13);
    drop(occupied);
    assert_eq!(budget.used(), 0);
    drop(moved);
}

#[test]
fn join_predicate_groups_reuse_masks_and_preserve_order_within_each_join() {
    fn tree(first: u32, count: u32, balanced: bool, joins: &mut Vec<u64>) -> crate::JoinPlan {
        let mut node = crate::JoinPlan {
            relations: 1_u64 << first,
            cardinality: 1.0,
            cost: 0.0,
            left: None,
            right: None,
            kind: None,
            join_edge: None,
        };
        if count > 1 {
            let split = if balanced { count / 2 } else { 1 };
            let left = tree(first, split, balanced, joins);
            let right = tree(first + split, count - split, balanced, joins);
            node.relations = left.relations | right.relations;
            node.left = Some(Box::new(left));
            node.right = Some(Box::new(right));
            joins.push(node.relations);
        }
        node
    }

    // Exercise both extreme tree depths, single-relation predicates, cross-subtree predicates and residuals.
    for balanced in [false, true] {
        let mut joins = Vec::new();
        let tree = tree(0, 64, balanced, &mut joins);
        let mut masks = Vec::new();
        for left in 0..64 {
            for right in 0..64 {
                masks.extend([1_u64 << left | 1_u64 << right, 0]);
            }
        }
        let mut positions = masks.clone();
        let mut groups = None;
        let budget = MemoryBudget::new(0);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let allocation = allocation_counter::measure(|| {
            groups = Some(PredicateGroups::assign(&tree, &mut positions, &control).unwrap());
        });
        assert_eq!(allocation.bytes_total, 0);
        let ranges: Vec<_> = groups.unwrap().ranges().collect();
        let mut next: Vec<_> = ranges.iter().map(|range| range.start).collect();
        for (mask, actual) in masks.into_iter().zip(positions) {
            let node = if mask == 0 {
                joins.len() - 1
            } else {
                joins
                    .iter()
                    .position(|&relations| mask & !relations == 0)
                    .unwrap()
            };
            assert_eq!(usize::try_from(actual).unwrap(), next[node]);
            next[node] += 1;
        }
        assert!(ranges
            .iter()
            .zip(next)
            .all(|(range, next)| range.end == next));
    }
}
