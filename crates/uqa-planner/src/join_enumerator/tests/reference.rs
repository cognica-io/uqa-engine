//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exhaustive ordered-tree reference independent of the subset memo and layers.

use super::super::*;

fn trees(graph: &JoinGraph, mask: u64) -> Vec<(f64, f64)> {
    if mask.is_power_of_two() {
        let index = mask.trailing_zeros() as usize;
        return vec![(graph.cardinalities[index], graph.access_costs[index])];
    }
    let mut results = Vec::new();
    let first = mask & mask.wrapping_neg();
    for left in 1..mask {
        if left & mask != left || left & first == 0 {
            continue;
        }
        let right = mask ^ left;
        let edges = graph.edges_between(left, right);
        if edges.is_empty() {
            continue;
        }
        for (left_rows, left_cost) in trees(graph, left) {
            for (right_rows, right_cost) in trees(graph, right) {
                let mut rows = left_rows * right_rows;
                for edge in &edges {
                    rows *= edge.selectivity;
                }
                let cost = CostEstimator::default()
                    .estimate_join(OperatorKind::HashJoinInner, left_rows, right_rows)
                    .total()
                    + left_cost
                    + right_cost;
                results.push((rows, cost));
            }
        }
    }
    results
}

#[test]
fn exact_decisions_match_exhaustive_connected_trees() {
    for count in 3..=6 {
        for dense in [false, true] {
            let mut graph = JoinGraph::new();
            for index in 0..count {
                graph
                    .add_relation_with_cost(
                        format!("r{index}"),
                        10.0 * (index + 1) as f64,
                        3.0 + index as f64,
                    )
                    .unwrap();
                for previous in 0..index {
                    if dense || previous + 1 == index || (previous == 0 && index == count - 1) {
                        graph.add_edge(previous, index, 0.125).unwrap();
                    }
                }
            }
            let expected = trees(&graph, graph.full_set())
                .into_iter()
                .map(|(_, cost)| cost)
                .min_by(f64::total_cmp)
                .unwrap();
            let actual = enumerate_dpccp(&graph).unwrap();
            assert!(
                (actual.cost - expected).abs() < 1e-9 * expected.max(1.0),
                "{actual:?}: expected {expected}"
            );
        }
    }
}

#[test]
fn disconnected_equal_cardinality_components_keep_discovery_order() {
    let mut graph = JoinGraph::new();
    for index in 0..8 {
        graph.add_relation(format!("r{index}"), 10.0).unwrap();
    }
    for index in 0..4 {
        graph.add_edge(index, 7 - index, 0.1).unwrap();
    }
    let plan = enumerate_dpccp(&graph).unwrap();
    let mut current = &plan;
    for expected in [0b0001_1000, 0b0010_0100, 0b0100_0010] {
        assert_eq!(current.kind, Some(OperatorKind::CrossJoin));
        assert_eq!(current.right.as_ref().unwrap().relations, expected);
        current = current.left.as_ref().unwrap();
    }
    assert_eq!(current.relations, 0b1000_0001);
}

#[test]
fn greedy_equal_cost_candidates_keep_relation_mask_order() {
    let mut graph = JoinGraph::new();
    for index in 0..18 {
        graph.add_relation(format!("r{index}"), 0.0).unwrap();
        if index > 0 {
            graph.add_edge(index - 1, index, 1.0).unwrap();
        }
    }
    let plan = enumerate_dpccp(&graph).unwrap();
    let mut current = &plan;
    for index in (1..18).rev() {
        assert_eq!(current.kind, Some(OperatorKind::HashJoinInner));
        assert_eq!(current.right.as_ref().unwrap().relations, 1_u64 << index);
        current = current.left.as_ref().unwrap();
    }
    assert_eq!(current.relations, 1);
}
