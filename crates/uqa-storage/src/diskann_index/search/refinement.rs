//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Already read nodes supply the navigation cutoff; lossy centroids do not stand in for their available coordinates.

use std::cmp::Ordering;

use uqa_core::memory::BudgetedBinaryHeap;

use super::Candidate;
use crate::diskann_index::{format::DiskANNNode, NavigationVector, SquaredNavigationDistance};
use crate::{read_control::StorageReadControl, StorageBackendResult};

struct Expanded {
    node: u64,
    distance: SquaredNavigationDistance,
}

impl PartialEq for Expanded {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other).is_eq()
    }
}

impl Eq for Expanded {}

impl PartialOrd for Expanded {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Expanded {
    fn cmp(&self, other: &Self) -> Ordering {
        self.distance
            .get()
            .total_cmp(&other.distance.get())
            .then_with(|| self.node.cmp(&other.node))
    }
}

pub(super) struct Refinement {
    query: NavigationVector,
    closest: BudgetedBinaryHeap<Expanded>,
    limit: usize,
}

impl Refinement {
    pub(super) fn new(
        query: &NavigationVector,
        limit: usize,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        Ok(Self {
            query: query.copy(control)?,
            closest: BudgetedBinaryHeap::new(control.memory()),
            limit,
        })
    }

    pub(super) fn allows(&self, candidate: &Candidate) -> bool {
        self.closest.len() < self.limit
            || self.closest.peek().is_some_and(|worst| {
                candidate
                    .distance
                    .get()
                    .total_cmp(&worst.distance.get())
                    .then_with(|| candidate.node.cmp(&worst.node))
                    .is_le()
            })
    }

    /// Each physical identity is offered exactly once, after its beam has been decoded successfully.
    pub(super) fn offer(
        &mut self,
        node: &DiskANNNode,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        let candidate = Expanded {
            node: node.node_id(),
            distance: self.query.squared_distance_to_raw(node.vector(), control)?,
        };
        if self.closest.len() == self.limit {
            if self.closest.peek().is_some_and(|worst| candidate >= *worst) {
                return Ok(());
            }
            self.closest.pop();
        }
        self.closest.push(candidate)?;
        Ok(())
    }
}
