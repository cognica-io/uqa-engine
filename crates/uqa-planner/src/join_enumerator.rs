//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! DPccp join enumeration (Moerkotte and Neumann, 2006). 1:1 port of
//! `uqa/planner/join_enumerator.py`.
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
//! O(1) hash-table lookup and set operations. The cost model uses
//! [`INDEX_JOIN_THRESHOLD`] to switch between an index-join estimate
//! (`smaller * log2(larger + 1)`) and a hash-join estimate
//! (`smaller + larger`), matching Python's heuristic.
//!
//! Returns a [`JoinPlan`] tree where each `Join` node records the
//! `(left, right, edge, cost, cardinality)` tuple. Disconnected join
//! graphs are handled by solving each connected component
//! independently and cross-joining them in cardinality-ascending
//! order.

use std::collections::BTreeMap;

use crate::cost_model::OperatorKind;
use crate::join_graph::{JoinEdge, JoinGraph};

/// Mirrors Python's `INDEX_JOIN_THRESHOLD`. When the smaller side has
/// fewer than this many rows, `_emit_csg_cmp_pair` favours the index
/// join cost shape `c_small * log2(c_large + 1)` over the symmetric
/// hash join cost.
pub const INDEX_JOIN_THRESHOLD: f64 = 100.0;

/// Mirrors Python's `MAX_DP_RELATIONS`. Beyond this count the exact
/// enumeration switches to the greedy fallback.
pub const MAX_DP_RELATIONS: usize = 16;

/// A (sub)plan for joining a set of relations. Mirrors Python's
/// `JoinPlan` dataclass: `relations` is the bitmask of relation
/// indices in the plan, `cardinality` and `cost` are the running
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
    fn leaf(idx: usize, rows: f64) -> Self {
        Self {
            relations: 1u64 << idx,
            cardinality: rows,
            cost: rows,
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

/// DPccp join order optimiser. Public so callers that need the
/// cancellation-friendly stages (`optimize`, `find_connected_components`)
/// can drive it directly. Mirrors Python's `DPccp` class.
pub struct DPccp<'g> {
    graph: &'g JoinGraph,
    dp: BTreeMap<u64, JoinPlan>,
    all_mask: u64,
}

impl<'g> DPccp<'g> {
    pub fn new(graph: &'g JoinGraph) -> Self {
        let n = graph.relation_count();
        let all_mask = if n == 0 { 0 } else { (1u64 << n) - 1 };
        Self {
            graph,
            dp: BTreeMap::new(),
            all_mask,
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
            return Some(JoinPlan::leaf(0, self.graph.cardinalities[0]));
        }
        // Initialise base relations.
        for i in 0..n {
            self.dp
                .insert(1u64 << i, JoinPlan::leaf(i, self.graph.cardinalities[i]));
        }
        if n > MAX_DP_RELATIONS {
            return Some(self.greedy_optimize());
        }
        self.enumerate_csg_cmp_pairs(n);
        if let Some(plan) = self.dp.get(&self.all_mask).cloned() {
            return Some(plan);
        }
        // Disconnected: cross-join the components.
        Some(self.join_disconnected_components())
    }

    /// Enumerate every connected subgraph / complement pair and feed
    /// them through `enumerate_splits`. Mirrors Python's
    /// `_enumerate_csg_cmp_pairs`.
    fn enumerate_csg_cmp_pairs(&mut self, n: usize) {
        let neighbors: Vec<Vec<usize>> = (0..n).map(|i| self.graph.neighbors(i)).collect();

        // `connected[mask]` is `true` iff the subgraph encoded by
        // `mask` is connected. The Python reference uses a bytearray;
        // a `Vec<bool>` is the closest Rust equivalent.
        let mut connected: Vec<bool> = vec![false; 1usize << n];
        let mut prev_layer: Vec<u64> = Vec::with_capacity(n);
        for i in 0..n {
            let mask = 1u64 << i;
            connected[mask as usize] = true;
            prev_layer.push(mask);
        }

        for _size in 2..=n {
            let mut cur_layer: Vec<u64> = Vec::new();
            for &s_mask in &prev_layer {
                let min_node = s_mask.trailing_zeros() as usize;
                let mut node = 0usize;
                let mut tmp = s_mask;
                while tmp != 0 {
                    if tmp & 1 == 1 {
                        for &nb in &neighbors[node] {
                            if nb > min_node && (s_mask & (1u64 << nb)) == 0 {
                                let new_mask = s_mask | (1u64 << nb);
                                if !connected[new_mask as usize] {
                                    connected[new_mask as usize] = true;
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

    /// Enumerate every canonical split `(s1, s2)` of `subset_mask`
    /// where `s1` contains the lowest set bit. Connectivity is
    /// checked via the `connected` table; only pairs that survive get
    /// fed through `emit_csg_cmp_pair`. Mirrors Python's
    /// `_enumerate_splits`.
    fn enumerate_splits(&mut self, subset_mask: u64, connected: &[bool]) {
        let lowest_bit = subset_mask & subset_mask.wrapping_neg();
        let rest = subset_mask ^ lowest_bit;

        // Iterate proper non-empty submasks of `rest`. Each `sub | lowest_bit`
        // forms a canonical S1 (containing the min element).
        let mut sub_rest = rest.wrapping_sub(1) & rest;
        while sub_rest != 0 {
            let sub = sub_rest | lowest_bit;
            let comp = subset_mask ^ sub;
            if connected[sub as usize] && connected[comp as usize] {
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
        if connected[rest as usize] {
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

    /// Cost a candidate join, install the best variant in the DP
    /// table. Mirrors Python's `_emit_csg_cmp_pair` exactly:
    /// cardinality is the cross-product times every edge's
    /// selectivity, and the cost gate prefers an index join when the
    /// smaller side fits inside `INDEX_JOIN_THRESHOLD`.
    fn emit_csg_cmp_pair(
        &mut self,
        plan1: &JoinPlan,
        plan2: &JoinPlan,
        edges: &[JoinEdge],
        combined_mask: u64,
    ) {
        let mut cardinality = plan1.cardinality * plan2.cardinality;
        for edge in edges {
            cardinality *= edge.selectivity;
        }
        let c1 = plan1.cardinality;
        let c2 = plan2.cardinality;
        let (join_cost, kind) = if c1 <= c2 {
            if c1 <= INDEX_JOIN_THRESHOLD {
                (c1 * (c2 + 1.0).log2(), OperatorKind::IndexJoin)
            } else {
                (c1 + c2, OperatorKind::HashJoinInner)
            }
        } else if c2 <= INDEX_JOIN_THRESHOLD {
            (c2 * (c1 + 1.0).log2(), OperatorKind::IndexJoin)
        } else {
            (c1 + c2, OperatorKind::HashJoinInner)
        };
        let total_cost = join_cost + plan1.cost + plan2.cost;
        let install = match self.dp.get(&combined_mask) {
            Some(existing) => total_cost < existing.cost,
            None => true,
        };
        if install {
            self.dp.insert(
                combined_mask,
                JoinPlan {
                    relations: plan1.relations | plan2.relations,
                    cardinality,
                    cost: total_cost,
                    left: Some(Box::new(plan1.clone())),
                    right: Some(Box::new(plan2.clone())),
                    join_edge: Some(edges[0].clone()),
                    kind: Some(kind),
                },
            );
        }
    }

    /// Cross-join every connected component in cardinality-ascending
    /// order. Mirrors Python's `_join_disconnected_components`.
    fn join_disconnected_components(&mut self) -> JoinPlan {
        let components = self.find_connected_components();
        let mut component_plans: Vec<JoinPlan> = Vec::with_capacity(components.len());
        for comp in &components {
            if comp.len() == 1 {
                let idx = *comp.iter().next().unwrap();
                let plan = self.dp.get(&(1u64 << idx)).cloned().expect("base plan");
                component_plans.push(plan);
                continue;
            }
            let mask: u64 = comp.iter().fold(0u64, |acc, i| acc | (1u64 << *i));
            if let Some(plan) = self.dp.get(&mask).cloned() {
                component_plans.push(plan);
                continue;
            }
            // Component was not solved; recurse on a sub-graph. This
            // path mirrors Python's defensive fallback.
            let original_indices: Vec<usize> = {
                let mut v: Vec<usize> = comp.clone();
                v.sort_unstable();
                v
            };
            let sub_graph = self.build_subgraph(&original_indices);
            let sub_plan = DPccp::new(&sub_graph)
                .optimize()
                .expect("non-empty component");
            component_plans.push(remap_plan(&sub_plan, &original_indices));
        }
        component_plans.sort_by(|a, b| {
            a.cardinality
                .partial_cmp(&b.cardinality)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let mut iter = component_plans.into_iter();
        let mut result = iter.next().expect("at least one component");
        for plan in iter {
            let combined = result.relations | plan.relations;
            let cardinality = result.cardinality * plan.cardinality;
            let cost = cardinality + result.cost + plan.cost;
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
        result
    }

    /// BFS the join graph to enumerate the connected components.
    /// Mirrors Python's `_find_connected_components`.
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

    /// Project a sub-graph containing only `nodes` (in their original
    /// indices). Edge bitmasks are remapped to the dense [0..k) range
    /// used by the recursive solve. Mirrors Python's `_build_subgraph`.
    fn build_subgraph(&self, nodes: &[usize]) -> JoinGraph {
        let mut sub = JoinGraph::new();
        let mut index_map: BTreeMap<usize, usize> = BTreeMap::new();
        for &old_idx in nodes {
            let new_idx = sub.add_relation(
                self.graph.relations[old_idx].clone(),
                self.graph.cardinalities[old_idx],
            );
            index_map.insert(old_idx, new_idx);
        }
        for edge in &self.graph.edges {
            let l_idx = edge.left.trailing_zeros() as usize;
            let r_idx = edge.right.trailing_zeros() as usize;
            if let (Some(&l_new), Some(&r_new)) = (index_map.get(&l_idx), index_map.get(&r_idx)) {
                sub.add_edge(l_new, r_new, edge.selectivity);
            }
        }
        sub
    }

    /// Greedy fallback for graphs with more than `MAX_DP_RELATIONS`
    /// relations: at every step pick the cheapest joinable pair until
    /// only one plan remains. `O(n^3)`. Mirrors Python's
    /// `_greedy_optimize`.
    fn greedy_optimize(self) -> JoinPlan {
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
                    let c1 = p1.cardinality;
                    let c2 = p2.cardinality;
                    let (greedy_join_cost, kind) = if c1 <= c2 {
                        if c1 <= INDEX_JOIN_THRESHOLD {
                            (c1 * (c2 + 1.0).log2(), OperatorKind::IndexJoin)
                        } else {
                            (c1 + c2, OperatorKind::HashJoinInner)
                        }
                    } else if c2 <= INDEX_JOIN_THRESHOLD {
                        (c2 * (c1 + 1.0).log2(), OperatorKind::IndexJoin)
                    } else {
                        (c1 + c2, OperatorKind::HashJoinInner)
                    };
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
                            join_edge: Some(edges[0].clone()),
                            kind: Some(kind),
                        });
                    }
                }
            }
            let Some(best_plan_unwrapped) = best_plan else {
                // No more joinable edges; cross-join the rest in
                // cardinality-ascending order, exactly as Python.
                let mut remaining: Vec<JoinPlan> = active.into_values().collect();
                remaining.sort_by(|a, b| {
                    a.cardinality
                        .partial_cmp(&b.cardinality)
                        .unwrap_or(std::cmp::Ordering::Equal)
                });
                let mut iter = remaining.into_iter();
                let mut result = iter.next().expect("at least one plan");
                for plan in iter {
                    let combined = result.relations | plan.relations;
                    let cardinality = result.cardinality * plan.cardinality;
                    let cost = cardinality + result.cost + plan.cost;
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
                return result;
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
        active
            .into_values()
            .next()
            .expect("non-empty greedy result")
    }
}

/// Remap relation indices in `plan` from a sub-graph's dense range
/// back to the parent graph's original indices. Mirrors Python's
/// `_remap_plan`.
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

    #[test]
    fn three_way_chain_picks_smallest_first() {
        let mut g = JoinGraph::new();
        let a = g.add_relation("a", 10.0);
        let b = g.add_relation("b", 100.0);
        let c = g.add_relation("c", 10_000.0);
        g.add_edge(a, b, 0.01);
        g.add_edge(b, c, 0.001);
        let plan = enumerate_dpccp(&g).unwrap();
        assert_eq!(plan.relations, 0b111);
        assert!(plan.left.is_some() && plan.right.is_some());
    }

    #[test]
    fn single_relation_returns_leaf() {
        let mut g = JoinGraph::new();
        g.add_relation("solo", 1.0);
        let plan = enumerate_dpccp(&g).unwrap();
        assert!(plan.left.is_none());
        assert!(plan.right.is_none());
        assert_eq!(plan.relations, 0b1);
    }

    #[test]
    fn empty_graph_returns_none() {
        let g = JoinGraph::new();
        assert!(enumerate_dpccp(&g).is_none());
    }

    #[test]
    fn disconnected_graph_cross_joins_components() {
        let mut g = JoinGraph::new();
        let a = g.add_relation("a", 50.0);
        let b = g.add_relation("b", 60.0);
        let c = g.add_relation("c", 70.0);
        g.add_edge(a, b, 0.5);
        // c is a disconnected component.
        let _ = c;
        let plan = enumerate_dpccp(&g).unwrap();
        // The cross-join must cover every relation.
        assert_eq!(plan.relations, 0b111);
    }

    #[test]
    fn star_query_picks_nested_plan() {
        // centre as the centre, leaf_b/c/d as leaves: every connects to
        // centre.
        let mut g = JoinGraph::new();
        let centre = g.add_relation("centre", 1_000.0);
        let leaf_b = g.add_relation("b", 10.0);
        let leaf_c = g.add_relation("c", 20.0);
        let leaf_d = g.add_relation("d", 30.0);
        g.add_edge(centre, leaf_b, 0.01);
        g.add_edge(centre, leaf_c, 0.01);
        g.add_edge(centre, leaf_d, 0.01);
        let plan = enumerate_dpccp(&g).unwrap();
        assert_eq!(plan.relations, 0b1111);
        assert!(plan.cost > 0.0);
    }

    #[test]
    fn greedy_fallback_kicks_in_above_threshold() {
        // With > MAX_DP_RELATIONS we expect the greedy fallback to
        // produce a plan that still covers every relation.
        let mut g = JoinGraph::new();
        let n = MAX_DP_RELATIONS + 2;
        let mut prev: usize = 0;
        for i in 0..n {
            let idx = g.add_relation(format!("t{i}"), 100.0);
            if i > 0 {
                g.add_edge(prev, idx, 0.05);
            }
            prev = idx;
        }
        let plan = enumerate_dpccp(&g).unwrap();
        assert_eq!(plan.relations.count_ones() as usize, n);
    }
}
