//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Dense node ranges use one bit per identity; sparse ranges retain bounded hash-set growth.

use super::{prepare::Control, types::NodeId};
use crate::vector_index::query::{QuerySet, VectorQueryBuffer};
use crate::StorageBackendResult;

pub(super) enum Visited {
    Dense {
        first: NodeId,
        words: VectorQueryBuffer<u64>,
    },
    Sparse(QuerySet<NodeId>),
}

impl Visited {
    pub(super) fn new(
        first: Option<NodeId>,
        last: Option<NodeId>,
        capacity: usize,
        control: Control<'_>,
    ) -> StorageBackendResult<Self> {
        super::prepare::check(control)?;
        let word_count = first
            .zip(last)
            .and_then(|(first, last)| last.checked_sub(first))
            .map(|range| range / 64 + 1)
            .and_then(|words| usize::try_from(words).ok());
        // A bitmap may occupy at most four words per initially reserved visited identity. This keeps initialization and memory proportional to search workspace even on very large or sparsely restored graphs.
        if let Some(word_count) = word_count.filter(|words| *words <= capacity.saturating_mul(4)) {
            let mut words = VectorQueryBuffer::new(control);
            words.reserve(word_count)?;
            for position in 0..word_count {
                if position.is_multiple_of(1024) {
                    super::prepare::check(control)?;
                }
                words.push(0)?;
            }
            return Ok(Self::Dense {
                first: first.expect("nonempty identity range"),
                words,
            });
        }
        Ok(Self::Sparse(QuerySet::with_capacity(capacity, control)?))
    }

    pub(super) fn insert(&mut self, id: NodeId) -> StorageBackendResult<bool> {
        match self {
            Self::Sparse(set) => set.insert(id),
            Self::Dense { first, words } => {
                // No stored node exists outside this immutable graph's identity range, so an out-of-range link cannot contribute a candidate.
                let Some(offset) = id.checked_sub(*first) else {
                    return Ok(false);
                };
                let Some(position) = usize::try_from(offset / 64)
                    .ok()
                    .and_then(|position| words.get_mut(position))
                else {
                    return Ok(false);
                };
                let mask = 1_u64 << (offset % 64);
                let fresh = *position & mask == 0;
                *position |= mask;
                Ok(fresh)
            }
        }
    }
}

#[cfg(test)]
mod tests;
