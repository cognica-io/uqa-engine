//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Original-vector distances decide whether a decoded node expands its edges; PQ only orders pending reads.

use std::cmp::Ordering;

use uqa_core::memory::BudgetedBinaryHeap;

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

    /// Each decoded identity is offered once in frozen beam order. Expand its edges only if it enters the nearest original-vector set at that point.
    pub(super) fn offer(
        &mut self,
        node: &DiskANNNode,
        control: &StorageReadControl,
    ) -> StorageBackendResult<bool> {
        let candidate = Expanded {
            node: node.node_id(),
            distance: self.query.squared_distance_to_raw(node.vector(), control)?,
        };
        if self.closest.len() == self.limit {
            if self.closest.peek().is_some_and(|worst| candidate >= *worst) {
                return Ok(false);
            }
            self.closest.pop();
        }
        self.closest.push(candidate)?;
        Ok(true)
    }
}
