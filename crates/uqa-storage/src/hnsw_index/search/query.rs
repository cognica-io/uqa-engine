//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! One immutable query reuses exact distances across hierarchy levels.

use super::{Candidate, HNSWIndex, NodeId};
use crate::hnsw_index::metric::distance;
use crate::StorageBackendResult;
use uqa_core::memory::{BudgetedHashMap, MemoryBudget, MemoryError};

// Occupancy is separate from the score so every IEEE bit pattern can be retained.
// Neighboring node IDs share a hash bucket and one budget reservation.
#[derive(Default)]
struct DistanceBlock {
    occupied: u32,
    scores: [f32; 32],
}

pub(in crate::hnsw_index) struct Query<'a> {
    values: &'a [f32],
    upper: Option<BudgetedHashMap<u64, DistanceBlock>>,
}

impl<'a> Query<'a> {
    pub(in crate::hnsw_index) fn new(values: &'a [f32], memory: &MemoryBudget) -> Self {
        let memory = memory.child((memory.limit() / 128).min(memory.available() / 4));
        Self {
            values,
            upper: Some(BudgetedHashMap::new(&memory)),
        }
    }

    pub(super) fn enter_base_layer(&mut self) {
        self.upper = None;
    }

    pub(in crate::hnsw_index) fn score(
        &mut self,
        index: &HNSWIndex,
        node_id: NodeId,
    ) -> StorageBackendResult<Option<Candidate>> {
        let block_id = node_id / 32;
        let offset = (node_id % 32) as usize;
        let bit = 1_u32 << offset;
        let cached = self
            .upper
            .as_mut()
            .and_then(|upper| upper.get_mut(&block_id));
        if let Some(block) = cached.as_deref() {
            if block.occupied & bit != 0 {
                return Ok(Some(Candidate {
                    distance: block.scores[offset],
                    node_id,
                }));
            }
        }
        let Some(vector) = index.normalized_vectors.get(u128::from(node_id))? else {
            return Ok(None);
        };
        let distance = distance(self.values, &vector.values);
        if let Some(block) = cached {
            block.scores[offset] = distance;
            block.occupied |= bit;
        } else if let Some(upper) = &mut self.upper {
            let mut block = DistanceBlock {
                occupied: bit,
                ..DistanceBlock::default()
            };
            block.scores[offset] = distance;
            match upper.insert(block_id, block) {
                Ok(_) | Err(MemoryError::Limit { .. }) => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(Some(Candidate { distance, node_id }))
    }
}
