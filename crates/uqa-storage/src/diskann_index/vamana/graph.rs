//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use uqa_core::memory::{BudgetedVec, MemoryError};

use super::{invalid, VamanaGraph, VamanaPoint};
use crate::{read_control::StorageReadControl, StorageBackendResult};

impl VamanaGraph {
    pub(super) fn empty(
        points: &[VamanaPoint<'_>],
        degree: usize,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        let degree = degree.min(points.len().saturating_sub(1));
        let edge_count = points
            .len()
            .checked_mul(degree)
            .ok_or(MemoryError::SizeOverflow)?;
        let mut keys = BudgetedVec::new(control.memory());
        let mut edges = BudgetedVec::new(control.memory());
        let mut lengths = BudgetedVec::new(control.memory());
        keys.reserve(points.len())?;
        edges.reserve(edge_count)?;
        lengths.reserve(points.len())?;
        for point in points {
            control.check()?;
            keys.push((point.doc_id, point.ordinal))?;
            lengths.push(0)?;
        }
        for index in 0..edge_count {
            super::super::metric::checkpoint(index, control)?;
            edges.push(0)?;
        }
        Ok(Self {
            keys,
            edges,
            lengths,
            degree,
            entry: None,
        })
    }

    pub(super) fn position(&self, node: u64) -> StorageBackendResult<usize> {
        usize::try_from(node)
            .ok()
            .filter(|&node| node < self.len())
            .ok_or_else(|| invalid("node ID is out of range"))
    }

    pub(super) fn replace(
        &mut self,
        node: u64,
        neighbors: &[u64],
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        control.check()?;
        let position = self.position(node)?;
        if neighbors.len() > self.degree {
            return Err(invalid("degree bound exceeded"));
        }
        for (index, &neighbor) in neighbors.iter().enumerate() {
            control.check()?;
            self.position(neighbor)?;
            if neighbor == node || neighbors[..index].contains(&neighbor) {
                return Err(invalid("self or duplicate edge"));
            }
        }
        let offset = position * self.degree;
        self.edges[offset..offset + neighbors.len()].copy_from_slice(neighbors);
        self.lengths[position] = neighbors.len();
        Ok(())
    }

    pub(super) fn append(
        &mut self,
        node: u64,
        neighbor: u64,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        control.check()?;
        let position = self.position(node)?;
        self.position(neighbor)?;
        if node == neighbor
            || self.neighbors(node)?.contains(&neighbor)
            || self.lengths[position] >= self.degree
        {
            return Err(invalid("invalid reverse edge"));
        }
        let offset = position * self.degree;
        self.edges[offset + self.lengths[position]] = neighbor;
        self.lengths[position] += 1;
        Ok(())
    }
}
