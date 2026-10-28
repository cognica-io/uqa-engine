//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::*;
use uqa_core::{
    memory::{MemoryBudget, MemoryError},
    CancellationToken,
};

fn graph(count: usize, shape: usize) -> JoinGraph {
    let mut graph = JoinGraph::new();
    for index in 0..count {
        graph
            .add_relation(format!("r{index}"), 10.0 + index as f64)
            .unwrap();
        for previous in 0..index {
            if match shape {
                0 => previous == 0,
                1 => true,
                2 => previous + 1 == index,
                _ => previous + 1 == index && index % 4 != 0,
            } {
                graph.add_edge(previous, index, 0.1).unwrap();
            }
        }
    }
    graph
}

fn same_plan(actual: &JoinPlan, expected: &JoinPlan) {
    assert_eq!(actual.relations, expected.relations);
    assert_eq!(actual.cardinality.to_bits(), expected.cardinality.to_bits());
    assert_eq!(actual.cost.to_bits(), expected.cost.to_bits());
    assert_eq!(actual.kind, expected.kind);
    match (&actual.join_edge, &expected.join_edge) {
        (Some(left), Some(right)) => {
            assert_eq!(left.left, right.left);
            assert_eq!(left.right, right.right);
            assert_eq!(left.selectivity.to_bits(), right.selectivity.to_bits());
        }
        (None, None) => {}
        _ => panic!("different selected join edge"),
    }
    for (actual, expected) in [
        (&actual.left, &expected.left),
        (&actual.right, &expected.right),
    ] {
        match (actual, expected) {
            (Some(actual), Some(expected)) => same_plan(actual, expected),
            (None, None) => {}
            _ => panic!("different selected children"),
        }
    }
}

#[test]
fn controlled_search_admits_scratch_and_retains_only_selected_output() {
    for (count, shape) in [(2, 0), (16, 0), (8, 1), (8, 2), (8, 3), (18, 2), (64, 3)] {
        let graph = graph(count, shape);
        let expected = enumerate_dpccp(&graph).unwrap();
        let budget = MemoryBudget::new(16 * 1024 * 1024);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        let mut output = None;
        let allocation = allocation_counter::measure(|| {
            output =
                enumerate_dpccp_with_control(&graph, CostEstimator::default(), &control).unwrap();
        });
        let output = output.unwrap();
        same_plan(&output, &expected);
        let retained = 2 * (count - 1) * size_of::<JoinPlan>();
        assert_eq!(output.reserved_bytes(), retained);
        assert_eq!(budget.used(), retained);
        assert_eq!(allocation.bytes_current as usize, retained);
        let required = budget.peak();
        drop(output);
        assert_eq!(budget.used(), 0);

        for limit in [0, 1, required / 2, required - 1, required] {
            let constrained = MemoryBudget::new(limit);
            let control = ProductionControl::new(&constrained, &original, &invoking);
            let result = enumerate_dpccp_with_control(&graph, CostEstimator::default(), &control);
            if limit < required {
                assert!(
                    matches!(
                        result,
                        Err(ValueRetentionError::Memory(MemoryError::Limit { .. }))
                    ),
                    "count={count}, shape={shape}, limit={limit}: {result:?}"
                );
            } else {
                same_plan(result.as_ref().unwrap().as_ref().unwrap(), &expected);
            }
            drop(result);
            assert_eq!(constrained.used(), 0);
        }
    }
}

#[test]
fn rejected_and_cancelled_search_allocates_no_candidate_storage() {
    let graph = graph(8, 1);
    for cancelled in 0..3 {
        let budget = MemoryBudget::new(if cancelled == 0 { 0 } else { usize::MAX });
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if cancelled == 1 {
            original.cancel();
        }
        if cancelled == 2 {
            invoking.cancel();
        }
        // Cancellation deferral consults Rust's lazily allocated thread
        // identity. Initialize that runtime state outside the producer count.
        let _ = original.check();
        let _ = invoking.check();
        let control = ProductionControl::new(&budget, &original, &invoking);
        let allocation = allocation_counter::measure(|| {
            let error = enumerate_dpccp_with_control(&graph, CostEstimator::default(), &control)
                .unwrap_err();
            if cancelled == 0 {
                assert!(matches!(
                    error,
                    ValueRetentionError::Memory(MemoryError::Limit { .. })
                ));
            } else {
                assert!(matches!(error, ValueRetentionError::Cancelled(_)));
            }
        });
        assert_eq!(allocation.count_total, 0, "{allocation:?}");
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn empty_and_single_relation_search_need_no_heap_storage() {
    for count in 0..=1 {
        let graph = graph(count, 0);
        let budget = MemoryBudget::new(0);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let allocation = allocation_counter::measure(|| {
            let result =
                enumerate_dpccp_with_control(&graph, CostEstimator::default(), &control).unwrap();
            assert_eq!(result.is_some(), count == 1);
            assert_eq!(budget.used(), 0);
        });
        assert_eq!(allocation.count_total, 0, "{allocation:?}");
    }
}

#[test]
fn search_respects_sibling_reservations_and_releases_on_unwind() {
    let graph = graph(8, 1);
    let budget = MemoryBudget::new(512 * 1024);
    let child = budget.child(512 * 1024);
    let sibling = budget.reserve(512 * 1024).unwrap();
    let token = CancellationToken::new();
    let control = ProductionControl::new(&child, &token, &token);
    assert!(enumerate_dpccp_with_control(&graph, CostEstimator::default(), &control).is_err());
    assert_eq!(child.used(), 0);
    assert_eq!(budget.used(), 512 * 1024);
    drop(sibling);
    let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _output =
            enumerate_dpccp_with_control(&graph, CostEstimator::default(), &control).unwrap();
        assert!(budget.used() > 0);
        panic!("consumer failed while retaining the selected tree");
    }));
    assert!(unwind.is_err());
    assert_eq!(child.used(), 0);
    assert_eq!(budget.used(), 0);
    let output = enumerate_dpccp_with_control(&graph, CostEstimator::default(), &control).unwrap();
    assert!(output.is_some());
    drop(output);
    assert_eq!(budget.used(), 0);
}
