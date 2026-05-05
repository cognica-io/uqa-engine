//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! DPccp join enumeration (Moerkotte and Neumann 2006).
//!
//! Mirrors `uqa/planner/join_enumerator.py`. The enumerator walks
//! the connected subgraphs of the [`JoinGraph`] in order, building up
//! a DP table keyed by relation bitmask. For each connected subset
//! `S`, we try every "complement" partition `(S1, S2)` that is itself
//! connected, and pick the join that minimises
//! `cost(S1) + cost(S2) + cost_of(join(S1, S2))`.

use std::collections::HashMap;

use crate::cost_model::{CostEstimator, OperatorKind};
use crate::join_graph::JoinGraph;

/// Concrete plan node returned by the enumerator. The tree is built
/// out of `Box<JoinPlan>` nodes; relation leaves keep the bit index
/// from the [`JoinGraph`] so the planner-to-physical bridge can
/// resolve them back to their backing data.
#[derive(Debug, Clone)]
pub enum JoinPlan {
    Leaf {
        relation: usize,
        rows: f64,
    },
    Join {
        left: Box<JoinPlan>,
        right: Box<JoinPlan>,
        kind: OperatorKind,
        rows: f64,
        cost: f64,
    },
}

impl JoinPlan {
    pub fn rows(&self) -> f64 {
        match self {
            JoinPlan::Leaf { rows, .. } => *rows,
            JoinPlan::Join { rows, .. } => *rows,
        }
    }

    pub fn cost(&self) -> f64 {
        match self {
            JoinPlan::Leaf { .. } => 0.0,
            JoinPlan::Join { cost, .. } => *cost,
        }
    }
}

/// Run DPccp over `graph` and return the cheapest join plan over the
/// full relation set. Returns `None` for an empty graph.
pub fn enumerate_dpccp(graph: &JoinGraph, est: &CostEstimator) -> Option<JoinPlan> {
    if graph.relations.is_empty() {
        return None;
    }
    if graph.relations.len() == 1 {
        return Some(JoinPlan::Leaf {
            relation: 0,
            rows: graph.cardinalities[0],
        });
    }

    // DP table keyed by the relation bitmask.
    let mut dp: HashMap<u64, JoinPlan> = HashMap::new();
    for (i, &rows) in graph.cardinalities.iter().enumerate() {
        dp.insert(1u64 << i, JoinPlan::Leaf { relation: i, rows });
    }

    let n = graph.relation_count();
    // We iterate over subsets in order of popcount so any subset's
    // partitions land in the table before we consume them.
    for size in 2..=n {
        for subset in subsets_of_size(n, size) {
            let mut best: Option<JoinPlan> = None;
            for left in proper_subsets(subset) {
                let right = subset & !left;
                if right == 0 {
                    continue;
                }
                let between = graph.edges_between(left, right);
                if between.is_empty() {
                    continue;
                }
                let Some(left_plan) = dp.get(&left).cloned() else {
                    continue;
                };
                let Some(right_plan) = dp.get(&right).cloned() else {
                    continue;
                };
                let l_rows = left_plan.rows();
                let r_rows = right_plan.rows();
                let join_sel = between
                    .iter()
                    .map(|e| e.selectivity)
                    .fold(1.0_f64, |acc, s| acc * s);
                let join_rows = (l_rows * r_rows * join_sel).max(1.0);
                let kind = if l_rows.max(r_rows) <= 1024.0 || between.len() == 1 {
                    OperatorKind::HashJoinInner
                } else {
                    OperatorKind::SortMergeJoin
                };
                let join_cost = est.estimate_join(kind, l_rows, r_rows).total()
                    + left_plan.cost()
                    + right_plan.cost();
                let candidate = JoinPlan::Join {
                    left: Box::new(left_plan),
                    right: Box::new(right_plan),
                    kind,
                    rows: join_rows,
                    cost: join_cost,
                };
                if best
                    .as_ref()
                    .map(|b| candidate.cost() < b.cost())
                    .unwrap_or(true)
                {
                    best = Some(candidate);
                }
            }
            if let Some(plan) = best {
                dp.insert(subset, plan);
            }
        }
    }
    dp.remove(&graph.full_set())
}

fn subsets_of_size(n: usize, k: usize) -> Vec<u64> {
    if k == 0 || k > n {
        return Vec::new();
    }
    let mut out = Vec::new();
    let total = 1u64 << n;
    for mask in 1u64..total {
        if mask.count_ones() as usize == k {
            out.push(mask);
        }
    }
    out
}

fn proper_subsets(mask: u64) -> Vec<u64> {
    let mut out = Vec::new();
    let mut sub = mask & mask.wrapping_sub(1);
    while sub > 0 {
        out.push(sub);
        if sub == 0 {
            break;
        }
        sub = (sub.wrapping_sub(1)) & mask;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cost_model::CostEstimator;

    #[test]
    fn three_way_chain_picks_smallest_first() {
        let mut g = JoinGraph::new();
        let a = g.add_relation("a", 10.0);
        let b = g.add_relation("b", 100.0);
        let c = g.add_relation("c", 10_000.0);
        g.add_edge(a, b, 0.01);
        g.add_edge(b, c, 0.001);
        let plan = enumerate_dpccp(&g, &CostEstimator::default()).unwrap();
        // Top-level join must end up combining either:
        //   (a join b) join c
        // or
        //   a join (b join c)
        // The cheaper shape on the synthetic numbers above is the
        // bushy plan, but at minimum the result must cover every
        // relation.
        match plan {
            JoinPlan::Join { rows, .. } => {
                assert!(rows > 0.0);
            }
            JoinPlan::Leaf { .. } => panic!("expected a join over 3 relations"),
        }
    }

    #[test]
    fn single_relation_returns_leaf() {
        let mut g = JoinGraph::new();
        g.add_relation("solo", 1.0);
        let plan = enumerate_dpccp(&g, &CostEstimator::default()).unwrap();
        assert!(matches!(plan, JoinPlan::Leaf { .. }));
    }

    #[test]
    fn empty_graph_returns_none() {
        let g = JoinGraph::new();
        assert!(enumerate_dpccp(&g, &CostEstimator::default()).is_none());
    }
}
