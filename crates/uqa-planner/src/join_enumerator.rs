//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! DPccp join enumeration following Moerkotte and Neumann (2006).
//!
//! Enumerates connected-subgraph / complement pairs of the
//! [`JoinGraph`] in canonical order: each connected subgraph S is
//! formed by extending a smaller connected subgraph with an adjacent
//! vertex whose index exceeds `min(S)`, ensuring each subgraph is
//! emitted exactly once. Complexity is `O(3^n)` over the relation
//! count; far below the `n!` of exhaustive enumeration. Falls back to
//! a greedy `O(n^3)` heuristic when the graph has more than
//! [`MAX_DP_RELATIONS`] relations.
//!
//! Internally relation subsets are encoded as `u64` bitmasks for
//! O(1) hash-table lookup and set operations. Equijoins are costed as
//! hash joins because that is the physical strategy available to the SQL
//! execution pipeline. An index-join cost must never influence ordering unless
//! the planner can prove that a compatible physical index join is executable.
//!
//! Returns a [`JoinPlan`] tree where each `Join` node records the
//! `(left, right, edge, cost, cardinality)` tuple. Disconnected join
//! graphs are handled by solving each connected component
//! independently and cross-joining them in cardinality-ascending
//! order.

use crate::cost_model::{CostEstimator, OperatorKind};
use crate::join_graph::{JoinEdge, JoinGraph};
use uqa_core::{
    memory::{Produced, ProductionControl},
    ValueRetentionError,
};

mod decision;
mod search;
#[cfg(test)]
mod tests;

/// Beyond this count, exact enumeration switches to the greedy fallback.
pub const MAX_DP_RELATIONS: usize = 16;

/// A (sub)plan for joining a set of relations. `relations` is the bitmask
/// of relation indices in the plan; `cardinality` and `cost` are the running
/// estimates, and `left` / `right` / `join_edge` are populated for
/// internal nodes.
#[derive(Debug, Clone)]
pub struct JoinPlan {
    pub relations: u64,
    pub cardinality: f64,
    pub cost: f64,
    pub left: Option<Box<JoinPlan>>,
    pub right: Option<Box<JoinPlan>>,
    pub join_edge: Option<JoinEdge>,
    /// The join algorithm `_emit_csg_cmp_pair` picked for this node.
    /// `None` for base relations and cross joins.
    pub kind: Option<OperatorKind>,
}

impl JoinPlan {
    pub fn rows(&self) -> f64 {
        self.cardinality
    }
    pub fn cost(&self) -> f64 {
        self.cost
    }
}

/// Run DPccp over `graph`; return the cheapest plan, or `None` for an empty graph.
pub fn enumerate_dpccp(graph: &JoinGraph) -> Option<JoinPlan> {
    DPccp::new(graph).optimize()
}

/// Run DPccp with an explicit physical cost estimator.
pub fn enumerate_dpccp_with_cost_estimator(
    graph: &JoinGraph,
    cost_estimator: CostEstimator,
) -> Option<JoinPlan> {
    DPccp::with_cost_estimator(graph, cost_estimator).optimize()
}

/// Admit subset memoization, temporary buffers and the selected output before
/// allocation. The borrowed graph keeps its original owner's storage; scratch
/// releases before return, and the selected tree retains its own reservation.
pub fn enumerate_dpccp_with_control(
    graph: &JoinGraph,
    cost_estimator: CostEstimator,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<JoinPlan>>, ValueRetentionError> {
    DPccp::with_cost_estimator(graph, cost_estimator).optimize_with_control(control)
}

/// DPccp join-order optimizer over a borrowed, validated join graph.
pub struct DPccp<'g> {
    graph: &'g JoinGraph,
    cost_estimator: CostEstimator,
}

impl<'g> DPccp<'g> {
    pub fn new(graph: &'g JoinGraph) -> Self {
        Self::with_cost_estimator(graph, CostEstimator::default())
    }

    pub fn with_cost_estimator(graph: &'g JoinGraph, cost_estimator: CostEstimator) -> Self {
        Self {
            graph,
            cost_estimator,
        }
    }

    pub fn optimize(self) -> Option<JoinPlan> {
        self.optimize_with_control(&ProductionControl::uncontrolled())
            .expect("uncontrolled join enumeration allocation failed")
            .map(|plan| {
                plan.into_uncontrolled()
                    .expect("ordinary enumeration has no lease")
            })
    }

    pub fn optimize_with_control(
        self,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Produced<JoinPlan>>, ValueRetentionError> {
        search::Search::new(self.graph, &self.cost_estimator, *control).optimize()
    }
}
