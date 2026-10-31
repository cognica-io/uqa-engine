//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::cost_model::CostCoefficients;

mod memory;
mod reference;

fn assert_executable_join_kinds(plan: &JoinPlan) {
    match (&plan.left, &plan.right) {
        (Some(left), Some(right)) => {
            if plan.join_edge.is_some() {
                assert_eq!(plan.kind, Some(OperatorKind::HashJoinInner));
            } else {
                assert_eq!(plan.kind, Some(OperatorKind::CrossJoin));
            }
            assert_executable_join_kinds(left);
            assert_executable_join_kinds(right);
        }
        (None, None) => assert!(plan.kind.is_none()),
        _ => panic!("join plan contains exactly one child: {plan:?}"),
    }
}

#[test]
fn three_way_chain_picks_smallest_first() {
    let mut g = JoinGraph::new();
    let a = g.add_relation("a", 10.0).unwrap();
    let b = g.add_relation("b", 100.0).unwrap();
    let c = g.add_relation("c", 10_000.0).unwrap();
    g.add_edge(a, b, 0.01).unwrap();
    g.add_edge(b, c, 0.001).unwrap();
    let plan = enumerate_dpccp(&g).unwrap();
    assert_eq!(plan.relations, 0b111);
    assert!(plan.left.is_some() && plan.right.is_some());
    assert_executable_join_kinds(&plan);
}

#[test]
fn single_relation_returns_leaf() {
    let mut g = JoinGraph::new();
    g.add_relation("solo", 1.0).unwrap();
    let plan = enumerate_dpccp(&g).unwrap();
    assert!(plan.left.is_none());
    assert!(plan.right.is_none());
    assert_eq!(plan.relations, 0b1);
}

#[test]
fn explicit_cost_estimator_drives_join_cost() {
    let mut graph = JoinGraph::new();
    let left = graph.add_relation_with_cost("left", 10.0, 7.0).unwrap();
    let right = graph.add_relation_with_cost("right", 100.0, 11.0).unwrap();
    graph.add_edge(left, right, 0.5).unwrap();

    let coefficients = CostCoefficients {
        hashjoin_build_per_row: 2.0,
        hashjoin_probe_per_row: 3.0,
        ..CostCoefficients::default()
    };
    let estimator = CostEstimator::new(coefficients);
    let expected_join_cost = estimator
        .estimate_join(OperatorKind::HashJoinInner, 10.0, 100.0)
        .total();

    let plan = enumerate_dpccp_with_cost_estimator(&graph, estimator).unwrap();

    assert_eq!(plan.cost, 7.0 + 11.0 + expected_join_cost);
}

#[test]
fn empty_graph_returns_none() {
    let g = JoinGraph::new();
    assert!(enumerate_dpccp(&g).is_none());
}

#[test]
fn dense_candidates_do_not_copy_partial_trees() {
    let mut graph = JoinGraph::new();
    for index in 0..8 {
        graph
            .add_relation(format!("r{index}"), 10.0 + index as f64)
            .unwrap();
        for previous in 0..index {
            graph.add_edge(previous, index, 0.1).unwrap();
        }
    }
    let allocation = allocation_counter::measure(|| {
        let plan = enumerate_dpccp(&graph).unwrap();
        assert_eq!(plan.relations, graph.full_set());
        assert_executable_join_kinds(&plan);
    });
    // 255 subset decisions and connectivity entries, bounded layers and one
    // selected 15-node tree fit without allocating per candidate split.
    assert!(allocation.bytes_total <= 128 * 1024, "{allocation:?}");
    assert!(allocation.count_total <= 1024, "{allocation:?}");
    assert_eq!(allocation.bytes_current, 0);
}

#[test]
fn disconnected_graph_cross_joins_components() {
    let mut g = JoinGraph::new();
    let a = g.add_relation("a", 50.0).unwrap();
    let b = g.add_relation("b", 60.0).unwrap();
    let c = g.add_relation("c", 70.0).unwrap();
    g.add_edge(a, b, 0.5).unwrap();
    // c is a disconnected component.
    let _ = c;
    let plan = enumerate_dpccp(&g).unwrap();
    // The cross-join must cover every relation.
    assert_eq!(plan.relations, 0b111);
    assert_executable_join_kinds(&plan);
}

#[test]
fn star_query_picks_nested_plan() {
    // centre as the centre, leaf_b/c/d as leaves: every connects to
    // centre.
    let mut g = JoinGraph::new();
    let centre = g.add_relation("centre", 1_000.0).unwrap();
    let leaf_b = g.add_relation("b", 10.0).unwrap();
    let leaf_c = g.add_relation("c", 20.0).unwrap();
    let leaf_d = g.add_relation("d", 30.0).unwrap();
    g.add_edge(centre, leaf_b, 0.01).unwrap();
    g.add_edge(centre, leaf_c, 0.01).unwrap();
    g.add_edge(centre, leaf_d, 0.01).unwrap();
    let plan = enumerate_dpccp(&g).unwrap();
    assert_eq!(plan.relations, 0b1111);
    assert!(plan.cost > 0.0);
    assert_executable_join_kinds(&plan);
}

#[test]
fn greedy_fallback_kicks_in_above_threshold() {
    // With > MAX_DP_RELATIONS we expect the greedy fallback to
    // produce a plan that still covers every relation.
    let mut g = JoinGraph::new();
    let n = MAX_DP_RELATIONS + 2;
    let mut prev: usize = 0;
    for i in 0..n {
        let idx = g.add_relation(format!("t{i}"), 100.0).unwrap();
        if i > 0 {
            g.add_edge(prev, idx, 0.05).unwrap();
        }
        prev = idx;
    }
    let plan = enumerate_dpccp(&g).unwrap();
    assert_eq!(plan.relations.count_ones() as usize, n);
    assert_executable_join_kinds(&plan);
}

#[test]
fn threshold_sized_star_uses_exact_plan() {
    let mut g = JoinGraph::new();
    let centre = g.add_relation("centre", 1_000.0).unwrap();
    for i in 1..MAX_DP_RELATIONS {
        let leaf = g
            .add_relation(format!("leaf{i}"), 100.0 + i as f64)
            .unwrap();
        g.add_edge(centre, leaf, 0.01).unwrap();
    }

    let plan = enumerate_dpccp(&g).unwrap();

    assert_eq!(plan.relations.count_ones() as usize, MAX_DP_RELATIONS);
    assert_eq!(plan.relations, g.full_set());
    assert!(plan.cost > 0.0);
}
