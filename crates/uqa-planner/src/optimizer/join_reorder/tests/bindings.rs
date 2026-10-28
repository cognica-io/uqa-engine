//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken, Value};

#[test]
fn predicate_slots_retain_the_buffer_lease_when_expressions_move() {
    let bytes = 4 * size_of::<JoinPredicateBinding>();
    let budget = MemoryBudget::new(bytes + 13);
    let occupied = budget.reserve(13).unwrap();
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let payload = "x".repeat(4 * 1024 * 1024);
    let address = payload.as_ptr();
    let mut output = None;
    let allocation = allocation_counter::measure(|| {
        let mut slots = PredicateBindings::new(4, &control).unwrap();
        slots.push(JoinPredicateBinding {
            expression: Some(ScalarExpr::Literal(Value::Str(payload))),
            relations: 0,
        });
        for index in 1..4 {
            slots.push(JoinPredicateBinding {
                expression: Some(ScalarExpr::Param(index)),
                relations: 0,
            });
        }
        output = Some(slots);
    });
    assert_eq!(allocation.bytes_total, u64::try_from(bytes).unwrap());
    assert_eq!(budget.used(), bytes + 13);
    let mut slots = output.unwrap();
    let moved = slots.as_mut_slice()[0].expression.take().unwrap();
    let ScalarExpr::Literal(Value::Str(payload)) = &moved else {
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
