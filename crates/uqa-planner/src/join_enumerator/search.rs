//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The existing subset visitation and physical costing over admitted decisions.

use super::{
    decision::{Decision, Memo},
    JoinEdge, JoinGraph, MAX_DP_RELATIONS,
};
use crate::cost_model::{CostEstimator, OperatorKind};
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionVec},
    ValueRetentionError,
};

mod star;

pub(super) struct Search<'g, 'a> {
    graph: &'g JoinGraph,
    cost: &'g CostEstimator,
    decisions: Memo<'a, Decision>,
    control: ProductionControl<'a>,
    // Graph admission limits the relation domain to 64; these masks need no
    // heap allocation and visit neighbors in their original ascending order.
    neighbors: [u64; 64],
}

impl<'g, 'a> Search<'g, 'a> {
    pub fn new(
        graph: &'g JoinGraph,
        cost: &'g CostEstimator,
        control: ProductionControl<'a>,
    ) -> Self {
        Self {
            graph,
            cost,
            decisions: Memo::new(control),
            control,
            neighbors: [0; 64],
        }
    }

    pub fn optimize(mut self) -> Result<Option<Produced<super::JoinPlan>>, ValueRetentionError> {
        self.control.check()?;
        let n = self.graph.relation_count();
        if n == 0 {
            return Ok(None);
        }
        if n == 1 {
            return self
                .control
                .finish(
                    super::JoinPlan {
                        relations: 1,
                        cardinality: self.graph.cardinalities[0],
                        cost: self.graph.access_costs[0],
                        left: None,
                        right: None,
                        join_edge: None,
                        kind: None,
                    },
                    self.control.empty_reservation(),
                )
                .map(Some);
        }
        for edge in &self.graph.edges {
            self.control.check()?;
            self.neighbors[edge.left.trailing_zeros() as usize] |= edge.right;
            self.neighbors[edge.right.trailing_zeros() as usize] |= edge.left;
        }
        for index in 0..n {
            let leaf = Decision::leaf(
                index,
                self.graph.cardinalities[index],
                self.graph.access_costs[index],
            );
            self.decisions.insert(leaf.relations, leaf)?;
        }
        let mask = if n > MAX_DP_RELATIONS {
            self.greedy(n)?
        } else if self.star(n)? {
            self.graph.full_set()
        } else {
            self.enumerate(n)?;
            if self.decisions.get(self.graph.full_set()).is_some() {
                self.graph.full_set()
            } else {
                self.disconnected()?
            }
        };
        self.decisions.produce(mask).map(Some)
    }

    fn edges(&self, left: u64, right: u64) -> impl Iterator<Item = &JoinEdge> {
        self.graph.edges.iter().filter(move |edge| {
            (edge.left & left != 0 && edge.right & right != 0)
                || (edge.right & left != 0 && edge.left & right != 0)
        })
    }

    fn join(
        &self,
        left: Decision,
        right: Decision,
    ) -> Result<Option<Decision>, ValueRetentionError> {
        self.control.check()?;
        let mut edges = self.edges(left.relations, right.relations).peekable();
        let Some(&&edge) = edges.peek() else {
            return Ok(None);
        };
        let mut cardinality = left.cardinality * right.cardinality;
        for edge in edges {
            self.control.check()?;
            cardinality *= edge.selectivity;
        }
        let kind = OperatorKind::HashJoinInner;
        let join_cost = self
            .cost
            .estimate_join(kind, left.cardinality, right.cardinality)
            .total();
        Ok(Some(Decision {
            relations: left.relations | right.relations,
            cardinality,
            cost: join_cost + left.cost + right.cost,
            left: left.relations,
            right: right.relations,
            edge: Some(edge),
            kind: Some(kind),
        }))
    }

    fn enumerate(&mut self, n: usize) -> Result<(), ValueRetentionError> {
        let mut connected = Memo::new(self.control);
        let mut previous = ProductionVec::new(self.control);
        previous.reserve(n)?;
        for index in 0..n {
            let mask = 1_u64 << index;
            connected.insert(mask, ())?;
            previous.push_copy(mask)?;
        }
        for _size in 2..=n {
            let mut current = ProductionVec::new(self.control);
            for &mask in previous.iter() {
                self.control.check()?;
                let lowest = mask.trailing_zeros() as usize;
                let mut nodes = mask;
                while nodes != 0 {
                    let node = nodes.trailing_zeros() as usize;
                    nodes &= nodes - 1;
                    let mut neighbors = self.neighbors[node] & !mask;
                    while neighbors != 0 {
                        let neighbor = neighbors.trailing_zeros() as usize;
                        neighbors &= neighbors - 1;
                        if neighbor <= lowest {
                            continue;
                        }
                        let combined = mask | (1_u64 << neighbor);
                        if connected.get(combined).is_none() {
                            connected.insert(combined, ())?;
                            current.push_copy(combined)?;
                        }
                    }
                }
            }
            for &mask in current.iter() {
                self.splits(mask, &connected)?;
            }
            previous = current;
        }
        Ok(())
    }

    fn splits(&mut self, mask: u64, connected: &Memo<'_, ()>) -> Result<(), ValueRetentionError> {
        let lowest = mask & mask.wrapping_neg();
        let rest = mask ^ lowest;
        let mut subset = rest.wrapping_sub(1) & rest;
        loop {
            self.control.check()?;
            let left_mask = subset | lowest;
            let right_mask = mask ^ left_mask;
            if connected.get(left_mask).is_some() && connected.get(right_mask).is_some() {
                if let (Some(left), Some(right)) = (
                    self.decisions.get(left_mask),
                    self.decisions.get(right_mask),
                ) {
                    if let Some(candidate) = self.join(left, right)? {
                        if self
                            .decisions
                            .get(mask)
                            .is_none_or(|existing| candidate.cost < existing.cost)
                        {
                            self.decisions.insert(mask, candidate)?;
                        }
                    }
                }
            }
            if subset == 0 {
                break;
            }
            subset = subset.wrapping_sub(1) & rest;
        }
        Ok(())
    }

    fn disconnected(&mut self) -> Result<u64, ValueRetentionError> {
        let mut remaining = self.graph.full_set();
        let mut plans = [Decision::EMPTY; 64];
        let mut count = 0;
        while remaining != 0 {
            self.control.check()?;
            let start = remaining & remaining.wrapping_neg();
            let mut component = start;
            let mut pending = start;
            while pending != 0 {
                let node = pending.trailing_zeros() as usize;
                pending &= pending - 1;
                let new = self.neighbors[node] & remaining & !component;
                component |= new;
                pending |= new;
            }
            remaining &= !component;
            // Enumeration visits every connected subset in cardinality order,
            // so a component already has its settled optimal decision.
            plans[count] = self
                .decisions
                .get(component)
                .expect("connected component was enumerated");
            count += 1;
        }
        self.cross_components(&mut plans[..count])
    }

    fn cross_components(&mut self, plans: &mut [Decision]) -> Result<u64, ValueRetentionError> {
        // Stable insertion order matters for equal rows. At most 64 components
        // exist; insertion sort uses no hidden stable-sort allocation.
        for index in 1..plans.len() {
            let mut position = index;
            while position > 0
                && plans[position]
                    .cardinality
                    .total_cmp(&plans[position - 1].cardinality)
                    .is_lt()
            {
                plans.swap(position, position - 1);
                position -= 1;
            }
        }
        let mut result = plans[0];
        for &right in &plans[1..] {
            self.control.check()?;
            let cost = self
                .cost
                .estimate_join(
                    OperatorKind::CrossJoin,
                    result.cardinality,
                    right.cardinality,
                )
                .total()
                + result.cost
                + right.cost;
            result = Decision {
                relations: result.relations | right.relations,
                cardinality: result.cardinality * right.cardinality,
                cost,
                left: result.relations,
                right: right.relations,
                edge: None,
                kind: Some(OperatorKind::CrossJoin),
            };
            self.decisions.insert(result.relations, result)?;
        }
        Ok(result.relations)
    }

    fn greedy(&mut self, n: usize) -> Result<u64, ValueRetentionError> {
        let mut active = [Decision::EMPTY; 64];
        for (index, item) in active[..n].iter_mut().enumerate() {
            *item = self
                .decisions
                .get(1_u64 << index)
                .expect("admitted base relation");
        }
        let mut count = n;
        while count > 1 {
            self.control.check()?;
            let mut best_cost = f64::INFINITY;
            let mut best = None;
            for left in 0..count {
                for right in left + 1..count {
                    if let Some(candidate) = self.join(active[left], active[right])? {
                        if candidate.cost < best_cost {
                            best_cost = candidate.cost;
                            best = Some((left, right, candidate));
                        }
                    }
                }
            }
            let Some((left, right, candidate)) = best else {
                return self.cross_components(&mut active[..count]);
            };
            self.decisions.insert(candidate.relations, candidate)?;
            active.copy_within(right + 1..count, right);
            count -= 1;
            active.copy_within(left + 1..count, left);
            active[count - 1] = candidate;
            active[..count].sort_unstable_by_key(|decision| decision.relations);
        }
        Ok(active[0].relations)
    }
}
