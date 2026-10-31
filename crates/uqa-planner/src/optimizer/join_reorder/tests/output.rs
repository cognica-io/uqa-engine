//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    materialization::{fixture, FunctionEstimates},
    *,
};
use uqa_core::{memory::MemoryBudget, CancellationToken};

fn equality(left: &str, right: &str, field: &str) -> ScalarExpr {
    ScalarExpr::Binary {
        op: BinaryOp::Equal,
        lhs: Box::new(ScalarExpr::QualifiedColumn {
            qualifier: left.into(),
            column: field.into(),
        }),
        rhs: Box::new(ScalarExpr::QualifiedColumn {
            qualifier: right.into(),
            column: field.into(),
        }),
    }
}

#[test]
fn inferred_scalars_and_output_buffers_retain_exact_live_allocation_leases() {
    let mut source = fixture(2, 1);
    let first = equality("s0", "s1", &"v".repeat(2 * 1024 * 1024));
    let second = equality("s0", "s1", "other");
    let budget = MemoryBudget::new(16 * 1024 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let mut selected = None;
    let allocation = allocation_counter::measure(|| {
        selected = reordered_inner_join_source(
            &mut source,
            &[&first, &second],
            &FunctionEstimates,
            &control,
        )
        .unwrap();
    });
    assert!(budget.used() > 4 * 1024 * 1024);
    assert_eq!(allocation.bytes_current, budget.used() as i64);
    let selected = selected.unwrap();
    let SourcePlan::Join {
        on: Some(ScalarExpr::And(items)),
        ..
    } = &*selected
    else {
        panic!("selected conjunction");
    };
    assert_eq!(items.len(), 2);
    assert!(items.contains(&first));
    assert!(items.contains(&second));
    drop(selected);
    assert_eq!(budget.used(), 0);
}

#[test]
fn inferred_payload_admission_rejects_before_copy_and_releases_partial_output() {
    let expression = equality("s0", "s1", &"v".repeat(4 * 1024 * 1024));
    for allowance in [1024 * 1024, 5 * 1024 * 1024] {
        let mut source = fixture(2, 1);
        let before = format!("{source:?}");
        let parent = MemoryBudget::new(allowance + 13);
        let occupied = parent.reserve(13).unwrap();
        let budget = parent.child(16 * 1024 * 1024);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let allocation = allocation_counter::measure(|| {
            let error = reordered_inner_join_source(
                &mut source,
                &[&expression],
                &FunctionEstimates,
                &control,
            )
            .unwrap_err();
            let crate::optimizer::OptimizerError::Expression(error) = error else {
                panic!("typed admission");
            };
            assert_eq!(error.sqlstate(), Some("53200"));
        });
        assert!(
            allocation.bytes_total < (allowance + 16 * 1024) as u64,
            "{allocation:?}"
        );
        assert_eq!(allocation.bytes_current, 0);
        assert_eq!(budget.used(), 0);
        assert_eq!(parent.used(), 13);
        assert_eq!(format!("{source:?}"), before);
        drop(occupied);
        assert_eq!(parent.used(), 0);
    }
}

fn nested_plan() -> UnifiedPlan {
    let mut source = fixture(3, 1);
    let SourcePlan::Join { left, on, .. } = &mut source else {
        panic!("root");
    };
    *on = Some(ScalarExpr::And(vec![
        equality("s0", "s2", "x"),
        equality("s0", "s2", "y"),
    ]));
    let SourcePlan::Join { on, .. } = left.as_mut() else {
        panic!("child");
    };
    *on = Some(ScalarExpr::And(vec![
        equality("s0", "s1", "x"),
        equality("s0", "s1", "y"),
    ]));
    let mut plan = UnifiedPlan::lower(uqa_sql::compile("SELECT 1").unwrap().remove(0));
    let UnifiedPlan::Query(query) = &mut plan else {
        panic!("query");
    };
    let RelationalPlan::QueryBlock(block) = &mut query.root else {
        panic!("block");
    };
    block.from = Some(source);
    plan
}

#[test]
fn parent_reordering_retires_consumed_child_conjunction_storage() {
    let plan = nested_plan();
    let budget = MemoryBudget::new(1024 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let mut selected = None;
    let allocation = allocation_counter::measure(|| {
        selected = Some(reorder_unified_plan_joins(plan, &FunctionEstimates, &control).unwrap());
    });
    // The input had two two-element AND buffers; their four slots were freed during the rewrite.
    let input_slots = 4 * size_of::<ScalarExpr>();
    assert!(budget.used() >= input_slots);
    assert_eq!(
        allocation.bytes_current + input_slots as i64,
        budget.used() as i64
    );
    let selected = selected.unwrap();
    assert_eq!(selected.reserved_bytes(), budget.used());
    drop(selected);
    assert_eq!(budget.used(), 0);
}

#[test]
fn parent_failure_releases_already_published_child_output() {
    struct Failure<'a> {
        calls: std::cell::Cell<usize>,
        token: &'a CancellationToken,
        unwind: bool,
    }
    impl SourceStatistics for Failure<'_> {
        fn relation_statistics(&self, _: &str) -> Option<crate::RelationStats> {
            None
        }
        fn source_access_estimate(
            &self,
            source: &SourcePlan,
        ) -> Option<crate::LocalAccessEstimate> {
            let calls = self.calls.get() + 1;
            self.calls.set(calls);
            if calls == 3 {
                if self.unwind {
                    panic!("parent callback unwind");
                }
                self.token.cancel();
            }
            FunctionEstimates.source_access_estimate(source)
        }
    }
    for failure in 0..3 {
        let plan = nested_plan();
        let budget = MemoryBudget::new(1024 * 1024);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let statistics = Failure {
            calls: std::cell::Cell::new(0),
            token: if failure == 0 { &original } else { &invoking },
            unwind: failure == 2,
        };
        let control = ProductionControl::new(&budget, &original, &invoking);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            reorder_unified_plan_joins(plan, &statistics, &control)
        }));
        if failure == 2 {
            assert!(result.is_err());
        } else {
            let crate::optimizer::OptimizerError::Expression(error) = result.unwrap().unwrap_err()
            else {
                panic!("typed cancellation");
            };
            assert_eq!(error.sqlstate(), Some("57014"));
        }
        assert!(statistics.calls.get() >= 3);
        assert_eq!(budget.used(), 0);
    }
}
