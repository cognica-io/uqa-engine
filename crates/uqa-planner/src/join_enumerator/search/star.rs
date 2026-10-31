//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exact star search with admitted states and a fixed-size predecessor walk.

use super::{ProductionVec, Search, ValueRetentionError};
use crate::cost_model::OperatorKind;

#[derive(Clone, Copy)]
struct StarState {
    cardinality: f64,
    cost: f64,
    previous: usize,
    leaf: usize,
}

impl Search<'_, '_> {
    pub(super) fn star(&mut self, n: usize) -> Result<bool, ValueRetentionError> {
        if n < 3
            || self.graph.edges.is_empty()
            || self.graph.edges.iter().any(|edge| edge.left == edge.right)
        {
            return Ok(false);
        }
        let mut centre = None;
        for (index, neighbors) in self.neighbors[..n].iter().enumerate() {
            if neighbors.count_ones() as usize == n - 1 {
                if centre.is_some() {
                    return Ok(false);
                }
                centre = Some(index);
            }
        }
        let Some(centre) = centre else {
            return Ok(false);
        };
        let centre_mask = 1_u64 << centre;
        if self
            .graph
            .edges
            .iter()
            .any(|edge| edge.left != centre_mask && edge.right != centre_mask)
        {
            return Ok(false);
        }
        let mut leaves = [0; 64];
        let mut count = 0;
        for index in 0..n {
            if index != centre {
                leaves[count] = index;
                count += 1;
            }
        }
        let states = 1_usize << count;
        let mut dp = ProductionVec::new(self.control);
        dp.reserve(states)?;
        for _ in 0..states {
            dp.push_copy(None)?;
        }
        dp.as_mut_slice()[0] = Some(StarState {
            cardinality: self.graph.cardinalities[centre],
            cost: self.graph.access_costs[centre],
            previous: 0,
            leaf: usize::MAX,
        });
        for mask in 0..states {
            self.control.check()?;
            let Some(base) = dp[mask] else {
                continue;
            };
            for (position, &leaf) in leaves[..count].iter().enumerate() {
                let bit = 1_usize << position;
                if mask & bit != 0 {
                    continue;
                }
                let leaf_rows = self.graph.cardinalities[leaf];
                let mut cardinality = base.cardinality * leaf_rows;
                for edge in self.edges(centre_mask, 1_u64 << leaf) {
                    self.control.check()?;
                    cardinality *= edge.selectivity;
                }
                let join_cost = self
                    .cost
                    .estimate_join(OperatorKind::HashJoinInner, base.cardinality, leaf_rows)
                    .total();
                let candidate = StarState {
                    cardinality,
                    cost: base.cost + self.graph.access_costs[leaf] + join_cost,
                    previous: mask,
                    leaf,
                };
                let next = mask | bit;
                if dp[next].is_none_or(|existing| candidate.cost < existing.cost) {
                    dp.as_mut_slice()[next] = Some(candidate);
                }
            }
        }
        let mut order = [0; 64];
        let mut length = 0;
        let mut mask = states - 1;
        while mask != 0 {
            self.control.check()?;
            let state = dp[mask].expect("every star subset is reachable");
            order[length] = state.leaf;
            length += 1;
            mask = state.previous;
        }
        let mut selected = self.decisions.get(centre_mask).expect("admitted centre");
        for &index in order[..length].iter().rev() {
            let leaf = self.decisions.get(1_u64 << index).expect("admitted leaf");
            selected = self
                .join(selected, leaf)?
                .expect("star leaf connects to centre");
            self.decisions.insert(selected.relations, selected)?;
        }
        Ok(true)
    }
}
