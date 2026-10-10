//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! One immutable query reuses exact distances across hierarchy levels.

use super::{Candidate, HNSWIndex, NodeId};
use crate::hnsw_index::metric::distance;
use crate::StorageBackendResult;
use uqa_core::memory::{BudgetedVec, MemoryBudget, MemoryError};

pub(in crate::hnsw_index) struct Query<'a> {
    values: &'a [f32],
    scores: BudgetedVec<Candidate>,
}

impl<'a> Query<'a> {
    pub(in crate::hnsw_index) fn new(values: &'a [f32], memory: &MemoryBudget) -> Self {
        let memory = memory.child((memory.limit() / 128).min(memory.available() / 4));
        Self {
            values,
            scores: BudgetedVec::new(&memory),
        }
    }

    pub(in crate::hnsw_index) fn score(
        &mut self,
        index: &HNSWIndex,
        node_id: NodeId,
    ) -> StorageBackendResult<Option<Candidate>> {
        if let Some(candidate) = self.scores.iter().find(|entry| entry.node_id == node_id) {
            return Ok(Some(*candidate));
        }
        let Some(vector) = index.normalized_vectors.get(u128::from(node_id))? else {
            return Ok(None);
        };
        let candidate = Candidate {
            distance: distance(self.values, &vector.values),
            node_id,
        };
        match self.scores.push(candidate) {
            Ok(()) | Err(MemoryError::Limit { .. }) => {}
            Err(error) => return Err(error.into()),
        }
        Ok(Some(candidate))
    }
}
