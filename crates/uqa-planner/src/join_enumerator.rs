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

use std::collections::{BTreeMap, BTreeSet};

use crate::cost_model::{CostEstimator, OperatorKind};
use crate::join_graph::{JoinEdge, JoinGraph};

/// Beyond this count, exact enumeration switches to the greedy fallback.
pub const MAX_DP_RELATIONS: usize = 16;

type StarLeaves = Vec<(usize, Vec<JoinEdge>)>;
type StarShape = (usize, StarLeaves);

#[derive(Clone, Copy)]
struct StarState {
    cardinality: f64,
    cost: f64,
    prev_mask: usize,
    leaf_pos: usize,
}

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
    /// Build a leaf plan for a single relation.
    fn leaf(idx: usize, rows: f64, access_cost: f64) -> Self {
        Self {
            relations: 1u64 << idx,
            cardinality: rows,
            cost: access_cost,
            left: None,
            right: None,
            join_edge: None,
            kind: None,
        }
    }

    /// Cardinality projected by this (sub)plan.
    pub fn rows(&self) -> f64 {
        self.cardinality
    }

    pub fn cost(&self) -> f64 {
        self.cost
    }
}

/// Run DPccp over `graph` and return the cheapest join plan over the
/// full relation set. Returns `None` for an empty graph.
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

/// DPccp join-order optimiser. Public so callers that need the
/// cancellation-friendly stages (`optimize`, `find_connected_components`)
/// can drive them directly.
pub struct DPccp<'g> {
    graph: &'g JoinGraph,
    dp: BTreeMap<u64, JoinPlan>,
    all_mask: u64,
    cost_estimator: CostEstimator,
}

impl<'g> DPccp<'g> {
    pub fn new(graph: &'g JoinGraph) -> Self {
        let all_mask = graph.full_set();
        Self {
            graph,
            dp: BTreeMap::new(),
            all_mask,
            cost_estimator: CostEstimator::default(),
        }
    }

    pub fn with_cost_estimator(graph: &'g JoinGraph, cost_estimator: CostEstimator) -> Self {
        let all_mask = graph.full_set();
        Self {
            graph,
            dp: BTreeMap::new(),
            all_mask,
            cost_estimator,
        }
    }

    /// Find the optimal join plan for the full relation set. Falls
    /// back to greedy for large queries. Returns `None` for empty
    /// graphs.
    pub fn optimize(mut self) -> Option<JoinPlan> {
        let n = self.graph.relation_count();
        if n == 0 {
            return None;
        }
        if n == 1 {
            return Some(JoinPlan::leaf(
                0,
                self.graph.cardinalities[0],
                self.graph.access_costs[0],
            ));
        }
        // Initialise base relations.
        for i in 0..n {
            self.dp.insert(
                1u64 << i,
                JoinPlan::leaf(i, self.graph.cardinalities[i], self.graph.access_costs[i]),
            );
        }
        if n <= MAX_DP_RELATIONS {
            if let Some(plan) = self.optimize_star(n) {
                return Some(plan);
            }
        }
        if n > MAX_DP_RELATIONS {
            return self.greedy_optimize();
        }
        self.enumerate_csg_cmp_pairs(n);
        if let Some(plan) = self.dp.get(&self.all_mask).cloned() {
            return Some(plan);
        }
        // Disconnected: cross-join the components.
        self.join_disconnected_components()
    }

    /// Enumerate every connected-subgraph/complement pair and feed it
    /// through `enumerate_splits`.
    fn enumerate_csg_cmp_pairs(&mut self, n: usize) {
        let neighbors: Vec<Vec<usize>> = (0..n).map(|i| self.graph.neighbors(i)).collect();

        // Keep connected subsets by their native u64 bitmask. This avoids
        // narrowing a mask to `usize` merely to address a dense side table.
        let mut connected = BTreeSet::new();
        let mut prev_layer: Vec<u64> = Vec::with_capacity(n);
        for i in 0..n {
            let mask = 1u64 << i;
            connected.insert(mask);
            prev_layer.push(mask);
        }

        for _size in 2..=n {
            let mut cur_layer: Vec<u64> = Vec::new();
            for &s_mask in &prev_layer {
                let Ok(min_node) = usize::try_from(s_mask.trailing_zeros()) else {
                    return;
                };
                let mut node = 0usize;
                let mut tmp = s_mask;
                while tmp != 0 {
                    if tmp & 1 == 1 {
                        for &nb in &neighbors[node] {
                            if nb > min_node && (s_mask & (1u64 << nb)) == 0 {
                                let new_mask = s_mask | (1u64 << nb);
                                if connected.insert(new_mask) {
                                    cur_layer.push(new_mask);
                                }
                            }
                        }
                    }
                    tmp >>= 1;
                    node += 1;
                }
            }
            for &subset_mask in &cur_layer {
                self.enumerate_splits(subset_mask, &connected);
            }
            prev_layer = cur_layer;
        }
    }

    fn optimize_star(&self, n: usize) -> Option<JoinPlan> {
        let (centre, leaves) = self.star_shape(n)?;
        let shift = u32::try_from(leaves.len()).ok()?;
        let states = 1usize.checked_shl(shift)?;
        let mut dp: Vec<Option<StarState>> = vec![None; states];
        dp[0] = Some(StarState {
            cardinality: self.graph.cardinalities[centre],
            cost: self.graph.access_costs[centre],
            prev_mask: 0,
            leaf_pos: usize::MAX,
        });

        for mask in 0..states {
            let Some(base) = dp[mask] else {
                continue;
            };
            for (leaf_pos, (leaf_idx, edges)) in leaves.iter().enumerate() {
                let bit = 1usize << leaf_pos;
                if mask & bit != 0 {
                    continue;
                }
                let leaf_cardinality = self.graph.cardinalities[*leaf_idx];
                let mut cardinality = base.cardinality * leaf_cardinality;
                for edge in edges {
                    cardinality *= edge.selectivity;
                }
                let join_cost = self.join_cost(base.cardinality, leaf_cardinality).0;
                let candidate = StarState {
                    cardinality,
                    cost: base.cost + self.graph.access_costs[*leaf_idx] + join_cost,
                    prev_mask: mask,
                    leaf_pos,
                };
                let next_mask = mask | bit;
                let install = match &dp[next_mask] {
                    Some(existing) => candidate.cost < existing.cost,
                    None => true,
                };
                if install {
                    dp[next_mask] = Some(candidate);
                }
            }
        }

        dp[states - 1]?;
        let mut order: Vec<usize> = Vec::with_capacity(leaves.len());
        let mut mask = states - 1;
        while mask != 0 {
            let state = dp[mask]?;
            order.push(state.leaf_pos);
            mask = state.prev_mask;
        }
        order.reverse();

        let mut plan = JoinPlan::leaf(
            centre,
            self.graph.cardinalities[centre],
            self.graph.access_costs[centre],
        );
        for leaf_pos in order {
            let (leaf_idx, edges) = &leaves[leaf_pos];
            let leaf = JoinPlan::leaf(
                *leaf_idx,
                self.graph.cardinalities[*leaf_idx],
                self.graph.access_costs[*leaf_idx],
            );
            plan = self.join_plans(&plan, &leaf, edges);
        }
        Some(plan)
    }

    fn star_shape(&self, n: usize) -> Option<StarShape> {
        if n < 3 || self.graph.edges.is_empty() {
            return None;
        }
        let mut neighbor_masks = vec![0u64; n];
        for edge in &self.graph.edges {
            if edge.left.count_ones() != 1 || edge.right.count_ones() != 1 {
                return None;
            }
            let left = usize::try_from(edge.left.trailing_zeros()).ok()?;
            let right = usize::try_from(edge.right.trailing_zeros()).ok()?;
            if left == right || left >= n || right >= n {
                return None;
            }
            neighbor_masks[left] |= edge.right;
            neighbor_masks[right] |= edge.left;
        }
        let candidates: Vec<usize> = neighbor_masks
            .iter()
            .enumerate()
            .filter_map(|(idx, mask)| {
                usize::try_from(mask.count_ones())
                    .ok()
                    .is_some_and(|count| count == n - 1)
                    .then_some(idx)
            })
            .collect();
        if candidates.len() != 1 {
            return None;
        }
        for centre in candidates {
            let centre_mask = 1u64 << centre;
            let mut by_leaf: BTreeMap<usize, Vec<JoinEdge>> = BTreeMap::new();
            let mut valid = true;
            for edge in &self.graph.edges {
                let leaf_mask = if edge.left == centre_mask {
                    edge.right
                } else if edge.right == centre_mask {
                    edge.left
                } else {
                    valid = false;
                    break;
                };
                if leaf_mask == 0 || leaf_mask == centre_mask {
                    valid = false;
                    break;
                }
                let leaf = usize::try_from(leaf_mask.trailing_zeros()).ok()?;
                by_leaf.entry(leaf).or_default().push(edge.clone());
            }
            if valid && by_leaf.len() == n - 1 {
                return Some((centre, by_leaf.into_iter().collect()));
            }
        }
        None
    }

    /// Enumerate every canonical split `(s1, s2)` of `subset_mask`
    /// where `s1` contains the lowest set bit. Connectivity is
    /// checked via the `connected` table; only pairs that survive get
    /// fed through `emit_csg_cmp_pair`.
    fn enumerate_splits(&mut self, subset_mask: u64, connected: &BTreeSet<u64>) {
        let lowest_bit = subset_mask & subset_mask.wrapping_neg();
        let rest = subset_mask ^ lowest_bit;

        // Iterate proper non-empty submasks of `rest`. Each `sub | lowest_bit`
        // forms a canonical S1 (containing the min element).
        let mut sub_rest = rest.wrapping_sub(1) & rest;
        while sub_rest != 0 {
            let sub = sub_rest | lowest_bit;
            let comp = subset_mask ^ sub;
            if connected.contains(&sub) && connected.contains(&comp) {
                if let (Some(plan1), Some(plan2)) =
                    (self.dp.get(&sub).cloned(), self.dp.get(&comp).cloned())
                {
                    let edges = self
                        .graph
                        .edges_between(plan1.relations, plan2.relations)
                        .into_iter()
                        .cloned()
                        .collect::<Vec<_>>();
                    if !edges.is_empty() {
                        self.emit_csg_cmp_pair(&plan1, &plan2, &edges, subset_mask);
                    }
                }
            }
            sub_rest = sub_rest.wrapping_sub(1) & rest;
        }
        // sub_rest == 0: S1 = {min element}, S2 = rest of subset.
        if connected.contains(&rest) {
            if let (Some(plan1), Some(plan2)) = (
                self.dp.get(&lowest_bit).cloned(),
                self.dp.get(&rest).cloned(),
            ) {
                let edges = self
                    .graph
                    .edges_between(plan1.relations, plan2.relations)
                    .into_iter()
                    .cloned()
                    .collect::<Vec<_>>();
                if !edges.is_empty() {
                    self.emit_csg_cmp_pair(&plan1, &plan2, &edges, subset_mask);
                }
            }
        }
    }

    /// Cost a candidate join and install the best variant in the DP table.
    /// Cardinality is the cross-product times every edge's selectivity. The
    /// physical SQL engine executes these equijoins as hash joins, so the
    /// enumerator uses the same cost shape and records the executable kind.
    fn emit_csg_cmp_pair(
        &mut self,
        plan1: &JoinPlan,
        plan2: &JoinPlan,
        edges: &[JoinEdge],
        combined_mask: u64,
    ) {
        let candidate = self.join_plans(plan1, plan2, edges);
        let install = match self.dp.get(&combined_mask) {
            Some(existing) => candidate.cost < existing.cost,
            None => true,
        };
        if install {
            self.dp.insert(combined_mask, candidate);
        }
    }

    fn join_plans(&self, plan1: &JoinPlan, plan2: &JoinPlan, edges: &[JoinEdge]) -> JoinPlan {
        let mut cardinality = plan1.cardinality * plan2.cardinality;
        for edge in edges {
            cardinality *= edge.selectivity;
        }
        let c1 = plan1.cardinality;
        let c2 = plan2.cardinality;
        let (join_cost, kind) = self.join_cost(c1, c2);
        JoinPlan {
            relations: plan1.relations | plan2.relations,
            cardinality,
            cost: join_cost + plan1.cost + plan2.cost,
            left: Some(Box::new(plan1.clone())),
            right: Some(Box::new(plan2.clone())),
            join_edge: edges.first().cloned(),
            kind: Some(kind),
        }
    }

    fn join_cost(&self, c1: f64, c2: f64) -> (f64, OperatorKind) {
        let kind = OperatorKind::HashJoinInner;
        (
            self.cost_estimator.estimate_join(kind, c1, c2).total(),
            kind,
        )
    }

    fn cross_join_cost(&self, c1: f64, c2: f64) -> f64 {
        self.cost_estimator
            .estimate_join(OperatorKind::CrossJoin, c1, c2)
            .total()
    }

    /// Cross-join every connected component in cardinality-ascending order.
    fn join_disconnected_components(&mut self) -> Option<JoinPlan> {
        let components = self.find_connected_components();
        let mut component_plans: Vec<JoinPlan> = Vec::with_capacity(components.len());
        for comp in &components {
            if comp.len() == 1 {
                let idx = *comp.first()?;
                let plan = self.dp.get(&(1u64 << idx)).cloned()?;
                component_plans.push(plan);
                continue;
            }
            let mask: u64 = comp.iter().fold(0u64, |acc, i| acc | (1u64 << *i));
            if let Some(plan) = self.dp.get(&mask).cloned() {
                component_plans.push(plan);
                continue;
            }
            // Component was not solved; recurse on a subgraph as a
            // defensive fallback.
            let original_indices: Vec<usize> = {
                let mut v: Vec<usize> = comp.clone();
                v.sort_unstable();
                v
            };
            let sub_graph = self.build_subgraph(&original_indices)?;
            let sub_plan =
                DPccp::with_cost_estimator(&sub_graph, self.cost_estimator.clone()).optimize()?;
            component_plans.push(remap_plan(&sub_plan, &original_indices));
        }
        component_plans.sort_by(|a, b| a.cardinality.total_cmp(&b.cardinality));
        let mut iter = component_plans.into_iter();
        let mut result = iter.next()?;
        for plan in iter {
            let combined = result.relations | plan.relations;
            let cardinality = result.cardinality * plan.cardinality;
            let cost = self.cross_join_cost(result.cardinality, plan.cardinality)
                + result.cost
                + plan.cost;
            result = JoinPlan {
                relations: combined,
                cardinality,
                cost,
                left: Some(Box::new(result)),
                right: Some(Box::new(plan)),
                join_edge: None,
                kind: Some(OperatorKind::CrossJoin),
            };
        }
        Some(result)
    }

    /// Use BFS to enumerate the join graph's connected components.
    fn find_connected_components(&self) -> Vec<Vec<usize>> {
        let n = self.graph.relation_count();
        let mut remaining: std::collections::BTreeSet<usize> = (0..n).collect();
        let mut components: Vec<Vec<usize>> = Vec::new();
        while let Some(&start) = remaining.iter().next() {
            let mut visited: std::collections::BTreeSet<usize> = std::collections::BTreeSet::new();
            visited.insert(start);
            let mut stack: Vec<usize> = vec![start];
            while let Some(node) = stack.pop() {
                for nb in self.graph.neighbors(node) {
                    if remaining.contains(&nb) && !visited.contains(&nb) {
                        visited.insert(nb);
                        stack.push(nb);
                    }
                }
            }
            for v in &visited {
                remaining.remove(v);
            }
            components.push(visited.into_iter().collect());
        }
        components
    }

    /// Project a subgraph containing only `nodes` in their original indices.
    /// Edge bitmasks are remapped to the dense `[0, k)` range used by the
    /// recursive solve.
    fn build_subgraph(&self, nodes: &[usize]) -> Option<JoinGraph> {
        let mut sub = JoinGraph::new();
        let mut index_map: BTreeMap<usize, usize> = BTreeMap::new();
        for &old_idx in nodes {
            let new_idx = sub.relations.len();
            sub.relations.push(self.graph.relations[old_idx].clone());
            sub.cardinalities.push(self.graph.cardinalities[old_idx]);
            sub.access_costs.push(self.graph.access_costs[old_idx]);
            index_map.insert(old_idx, new_idx);
        }
        for edge in &self.graph.edges {
            let l_idx = usize::try_from(edge.left.trailing_zeros()).ok()?;
            let r_idx = usize::try_from(edge.right.trailing_zeros()).ok()?;
            if let (Some(&l_new), Some(&r_new)) = (index_map.get(&l_idx), index_map.get(&r_idx)) {
                sub.edges.push(JoinEdge {
                    left: 1_u64 << l_new,
                    right: 1_u64 << r_new,
                    selectivity: edge.selectivity,
                });
            }
        }
        Some(sub)
    }

    /// Greedy fallback for graphs with more than `MAX_DP_RELATIONS`
    /// relations: at every step, pick the cheapest joinable pair until
    /// only one plan remains. `O(n^3)`.
    fn greedy_optimize(self) -> Option<JoinPlan> {
        let mut active: BTreeMap<u64, JoinPlan> = self.dp.clone();
        while active.len() > 1 {
            let mut best_cost = f64::INFINITY;
            let mut best_combined_mask: u64 = 0;
            let mut best_plan: Option<JoinPlan> = None;
            let items: Vec<(u64, JoinPlan)> = active.iter().map(|(k, v)| (*k, v.clone())).collect();
            for i in 0..items.len() {
                let (m1, ref p1) = items[i];
                for (m2, p2) in items.iter().skip(i + 1) {
                    let edges = self
                        .graph
                        .edges_between(p1.relations, p2.relations)
                        .into_iter()
                        .cloned()
                        .collect::<Vec<_>>();
                    if edges.is_empty() {
                        continue;
                    }
                    let mut cardinality = p1.cardinality * p2.cardinality;
                    for edge in &edges {
                        cardinality *= edge.selectivity;
                    }
                    let (greedy_join_cost, kind) = self.join_cost(p1.cardinality, p2.cardinality);
                    let cost = greedy_join_cost + p1.cost + p2.cost;
                    if cost < best_cost {
                        best_cost = cost;
                        best_combined_mask = m1 | m2;
                        best_plan = Some(JoinPlan {
                            relations: p1.relations | p2.relations,
                            cardinality,
                            cost,
                            left: Some(Box::new(p1.clone())),
                            right: Some(Box::new(p2.clone())),
                            join_edge: edges.first().cloned(),
                            kind: Some(kind),
                        });
                    }
                }
            }
            let Some(best_plan_unwrapped) = best_plan else {
                // No more joinable edges; cross-join the rest in
                // cardinality-ascending order.
                let mut remaining: Vec<JoinPlan> = active.into_values().collect();
                remaining.sort_by(|a, b| a.cardinality.total_cmp(&b.cardinality));
                let mut iter = remaining.into_iter();
                let mut result = iter.next()?;
                for plan in iter {
                    let combined = result.relations | plan.relations;
                    let cardinality = result.cardinality * plan.cardinality;
                    let cost = self.cross_join_cost(result.cardinality, plan.cardinality)
                        + result.cost
                        + plan.cost;
                    result = JoinPlan {
                        relations: combined,
                        cardinality,
                        cost,
                        left: Some(Box::new(result)),
                        right: Some(Box::new(plan)),
                        join_edge: None,
                        kind: Some(OperatorKind::CrossJoin),
                    };
                }
                return Some(result);
            };
            // Drop every plan whose mask is fully contained in the
            // newly merged mask, then insert the merged plan.
            let drop: Vec<u64> = active
                .keys()
                .copied()
                .filter(|rel_mask| rel_mask & best_combined_mask == *rel_mask)
                .collect();
            for k in drop {
                active.remove(&k);
            }
            active.insert(best_combined_mask, best_plan_unwrapped);
        }
        active.into_values().next()
    }
}

/// Remap relation indices in `plan` from a subgraph's dense range back to
/// the parent graph's original indices.
fn remap_plan(plan: &JoinPlan, original_indices: &[usize]) -> JoinPlan {
    let new_relations = remap_mask(plan.relations, original_indices);
    JoinPlan {
        relations: new_relations,
        cardinality: plan.cardinality,
        cost: plan.cost,
        left: plan
            .left
            .as_deref()
            .map(|l| Box::new(remap_plan(l, original_indices))),
        right: plan
            .right
            .as_deref()
            .map(|r| Box::new(remap_plan(r, original_indices))),
        join_edge: plan.join_edge.clone(),
        kind: plan.kind,
    }
}

fn remap_mask(mask: u64, original_indices: &[usize]) -> u64 {
    let mut out = 0u64;
    let mut m = mask;
    let mut i = 0;
    while m != 0 {
        if m & 1 == 1 {
            out |= 1u64 << original_indices[i];
        }
        m >>= 1;
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cost_model::CostCoefficients;

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
}
