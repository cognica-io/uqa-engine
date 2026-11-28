//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A bounded construction frontier retains the complete, separately charged set of expanded nodes.

use uqa_core::memory::{BudgetedVec, MemoryError};

use super::{invalid, VamanaGraph, VamanaPoint};
use crate::{read_control::StorageReadControl, StorageBackendResult};

const IN_FRONTIER: u8 = 1;
const EXPANDED: u8 = 2;

pub(super) struct Workspace {
    capacity: usize,
    frontier: BudgetedVec<(u64, f64)>,
    membership: BudgetedVec<u8>,
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
        let mut membership = BudgetedVec::new(control.memory());
        membership.reserve(count)?;
        for index in 0..count {
            super::super::metric::checkpoint(index, control)?;
            membership.push(0)?;
        }
        Ok(Self {
            capacity,
            frontier,
            membership,
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
        for (index, &(node, _)) in self.frontier.iter().enumerate() {
            super::super::metric::checkpoint(index, control)?;
            self.membership[node as usize] = 0;
        }
        self.frontier.clear();
        for (index, &node) in self.visited.iter().enumerate() {
            super::super::metric::checkpoint(index, control)?;
            self.membership[node as usize] = 0;
        }
        self.visited.clear();
        let entry = graph
            .entry
            .ok_or_else(|| invalid("missing construction entry"))?;
        self.admit(entry, points, query, control)?;
        while let Some(&(current, _)) = self
            .frontier
            .iter()
            .find(|(node, _)| self.membership[*node as usize] & EXPANDED == 0)
        {
            control.check()?;
            self.visited.push(current)?;
            self.membership[current as usize] |= EXPANDED;
            for &neighbor in graph.neighbors(current)? {
                self.admit(neighbor, points, query, control)?;
            }
            self.frontier
                .sort_unstable_by(|a, b| a.1.total_cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
            for (index, &(node, _)) in self.frontier.iter().skip(self.capacity).enumerate() {
                super::super::metric::checkpoint(index, control)?;
                self.membership[node as usize] &= !IN_FRONTIER;
            }
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
        if self.membership[candidate] & IN_FRONTIER != 0 {
            return Ok(());
        }
        let distance = points[candidate]
            .vector
            .squared_distance(points[query].vector, control)?
            .get();
        self.frontier.push((node, distance))?;
        self.membership[candidate] |= IN_FRONTIER;
        Ok(())
    }
}
