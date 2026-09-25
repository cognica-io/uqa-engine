//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A bounded construction frontier retains the complete, separately charged set of expanded nodes.

use uqa_core::memory::{BudgetedVec, MemoryError};

use super::{invalid, VamanaGraph, VamanaPoint};
use crate::{read_control::StorageReadControl, StorageBackendResult};

pub(super) struct Workspace {
    capacity: usize,
    frontier: BudgetedVec<(u64, f64)>,
    expanded: BudgetedVec<bool>,
    visited: BudgetedVec<u64>,
}

impl Workspace {
    pub(super) fn new(
        count: usize,
        capacity: usize,
        degree: usize,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        if capacity == 0 {
            return Err(invalid("construction list must be positive"));
        }
        let capacity = capacity.min(count);
        let mut frontier = BudgetedVec::new(control.memory());
        frontier.reserve(
            capacity
                .checked_add(degree)
                .ok_or(MemoryError::SizeOverflow)?
                .min(count),
        )?;
        let mut expanded = BudgetedVec::new(control.memory());
        expanded.reserve(count)?;
        for index in 0..count {
            super::super::metric::checkpoint(index, control)?;
            expanded.push(false)?;
        }
        Ok(Self {
            capacity,
            frontier,
            expanded,
            visited: BudgetedVec::new(control.memory()),
        })
    }

    pub(super) fn visited(
        &mut self,
        graph: &VamanaGraph,
        points: &[VamanaPoint<'_>],
        query: u64,
        control: &StorageReadControl,
    ) -> StorageBackendResult<&[u64]> {
        control.check()?;
        let query = super::prune::position(points, query)?;
        self.frontier.clear();
        self.visited.clear();
        for chunk in self.expanded.chunks_mut(1024) {
            control.check()?;
            chunk.fill(false);
        }
        let entry = graph
            .entry
            .ok_or_else(|| invalid("missing construction entry"))?;
        self.admit(entry, points, query, control)?;
        while let Some(&(current, _)) = self
            .frontier
            .iter()
            .find(|(node, _)| !self.expanded[*node as usize])
        {
            control.check()?;
            self.expanded[current as usize] = true;
            self.visited.push(current)?;
            for &neighbor in graph.neighbors(current)? {
                if !self.frontier.iter().any(|(node, _)| *node == neighbor) {
                    self.admit(neighbor, points, query, control)?;
                }
            }
            self.frontier
                .sort_unstable_by(|a, b| a.1.total_cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
            self.frontier.truncate(self.capacity);
        }
        Ok(&self.visited)
    }

    fn admit(
        &mut self,
        node: u64,
        points: &[VamanaPoint<'_>],
        query: usize,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        let candidate = super::prune::position(points, node)?;
        let distance = points[candidate]
            .vector
            .squared_distance(points[query].vector, control)?
            .get();
        self.frontier.push((node, distance))?;
        Ok(())
    }
}
