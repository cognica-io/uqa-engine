//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ast::{AssignmentStep, AssignmentTarget, AssignmentTargets},
    ir::{RetainedConstantValue, RetainedScalarLiteral},
    plan::{
        retirement::{tests::ScalarSource, Pending},
        AssignmentPlan, MergeWhenPlan,
    },
    ScalarExpr,
};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use uqa_core::memory::MemoryBudget;

#[test]
fn scalar_field_retirement_preserves_order_at_the_deepest_native_container_layout() {
    let native = MemoryBudget::new(1 << 20);
    let sibling = native.reserve(37).unwrap();
    let mut observed = Vec::new();
    let mut scalar = || {
        let retired = Arc::new(AtomicUsize::new(0));
        observed.push(retired.clone());
        ScalarExpr::RetainedLiteral(RetainedScalarLiteral::new(
            RetainedConstantValue::new(
                Arc::new(ScalarSource {
                    native: native.clone(),
                    minimum: Arc::new(AtomicUsize::new(37)),
                    retired,
                }),
                0,
            ),
            None,
            None,
            None,
        ))
    };
    let clauses = (0..2)
        .map(|_| {
            let condition = Some(scalar());
            let assignments = (0..2)
                .map(|_| {
                    let targets = (0..2)
                        .map(|_| AssignmentTarget {
                            column: "array_field".to_owned(),
                            indirection: (0..2)
                                .map(|_| AssignmentStep::Index(Box::new(scalar())))
                                .collect(),
                        })
                        .collect::<Vec<_>>();
                    AssignmentPlan {
                        target: AssignmentTargets::Multiple(targets.into()),
                        value: scalar(),
                    }
                })
                .collect();
            MergeWhenPlan::UpdateMatched {
                condition,
                assignments,
            }
        })
        .collect::<Vec<_>>();
    let mut clauses = Pending::new(clauses);
    let mut retirement = ScalarRetirement::default();
    let mut released = 0;
    let mut complete = false;
    for _ in 0..1000 {
        assert_eq!(
            allocation_counter::measure(|| complete = retirement.advance(&mut *clauses))
                .count_total,
            0
        );
        let count = observed
            .iter()
            .filter(|value| value.load(Ordering::Relaxed) != 0)
            .count();
        assert!(count - released <= 1);
        assert!(observed[..count]
            .iter()
            .all(|value| value.load(Ordering::Relaxed) == 1));
        assert!(observed[count..]
            .iter()
            .all(|value| value.load(Ordering::Relaxed) == 0));
        released = count;
        if complete {
            break;
        }
    }
    assert!(complete);
    assert_eq!(released, 22);
    assert!(clauses.is_empty());
    assert_eq!(
        allocation_counter::measure(|| drop((retirement, clauses))).count_total,
        0
    );
    assert_eq!(native.used(), sibling.bytes());
}
