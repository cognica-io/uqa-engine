//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::plan::{retirement::construction::Context, QueryScalarSlot, UnifiedPlan};

fn observe(budget: &MemoryBudget, excluded: usize, bytes: impl Fn() -> usize) {
    let retained = budget.used();
    let occupied = budget.reserve(budget.available()).unwrap();
    let observed = allocation_counter::measure(|| {
        for _ in 0..32 {
            assert_eq!(bytes(), retained - excluded);
        }
    });
    assert_eq!((observed.count_total, observed.bytes_current), (0, 0));
    assert_eq!(budget.used(), retained + occupied.bytes());
    drop(occupied);
}

#[test]
fn scalar_footprint_is_exact_without_scratch_on_deep_unary_and_branching_owners() {
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            let budget = MemoryBudget::new(64 << 20);
            let sibling = budget.reserve(37).unwrap();
            let token = CancellationToken::new();
            let control = ProductionControl::new(&budget, &token, &token);
            for branching in [false, true] {
                let mut value = super::retirement::deep(&control);
                if branching {
                    for _ in 0..8192 {
                        let sibling = text(17).copy_for_rewrite(&control).unwrap();
                        value =
                            OwnedScalarExpr::boolean([value, sibling].into_iter(), true, &control)
                                .unwrap();
                    }
                }
                observe(&budget, 37, || value.reserved_bytes());
                let replacement = text(53).copy_for_rewrite(&control).unwrap();
                let (mut value, previous) = value.replace_child(0, replacement, &control).unwrap();
                observe(&budget, 37, || {
                    value.reserved_bytes() + previous.reserved_bytes()
                });
                let replacement = value.select_child(0);
                observe(&budget, 37, || {
                    replacement.reserved_bytes() + previous.reserved_bytes()
                });
                assert_eq!(replacement.reserved_bytes(), 53);
                value = previous;
                for _ in 0..32 {
                    value = value.select_child(0);
                    observe(&budget, 37, || {
                        replacement.reserved_bytes() + value.reserved_bytes()
                    });
                }
                let released = allocation_counter::measure(|| drop((value, replacement)));
                assert_eq!(released.count_total, 0);
                assert_eq!(budget.used(), 37);
            }
            drop(sibling);
            assert_eq!(budget.used(), 0);
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn enclosing_query_observes_deep_scalar_footprint_after_slot_transfer_and_cancellation() {
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            let budget = MemoryBudget::new(64 << 20);
            let sibling = budget.reserve(37).unwrap();
            let token = CancellationToken::new();
            let control = ProductionControl::new(&budget, &token, &token);
            let context = Context::new(&control).unwrap().child(&control).unwrap();
            let excluded = budget.used();
            let UnifiedPlan::Query(source) =
                UnifiedPlan::lower(crate::compile("SELECT 1").unwrap().remove(0))
            else {
                panic!("query")
            };
            let scalar = super::retirement::deep(&control);
            let query = source
                .copy_for_rewrite(&control)
                .unwrap()
                .map_scalar(QueryScalarSlot::Projection(0), &control, |_| Ok(scalar))
                .unwrap();
            let query = context.retain(query);
            observe(&budget, excluded, || query.reserved_bytes());
            token.cancel();
            observe(&budget, excluded, || query.reserved_bytes());
            let released = allocation_counter::measure(|| drop(query));
            assert_eq!(released.count_total, 0);
            assert_eq!(budget.used(), excluded);
            drop(context);
            assert_eq!(budget.used(), 37);
            drop(sibling);
            assert_eq!(budget.used(), 0);
        })
        .unwrap()
        .join()
        .unwrap();
}
