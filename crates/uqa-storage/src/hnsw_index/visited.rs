//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Membership uses a bounded bitmap or spillable sparse words without changing visited identities.

use super::{prepare::Control, store::Map, types::NodeId};
use crate::{
    read_control::StorageReadControl, vector_index::query::VectorQueryBuffer, StorageBackendError,
    StorageBackendResult,
};
use uqa_core::memory::MemoryError;

pub(super) struct Sparse {
    words: Map<u64>,
    control: StorageReadControl,
}

pub(super) enum Visited {
    Dense {
        first: NodeId,
        words: VectorQueryBuffer<u64>,
    },
    Sparse(Sparse),
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
        if let Some(word_count) = word_count.filter(|words| {
            *words <= capacity.saturating_mul(4)
                && control.is_none_or(|control| {
                    *words <= (control.memory().limit() / 16).max(64) / size_of::<u64>()
                })
        }) {
            let mut words = VectorQueryBuffer::new(control);
            match words.reserve(word_count) {
                Ok(()) => {
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
                Err(StorageBackendError::Memory(MemoryError::Limit { .. })) => (),
                Err(error) => return Err(error),
            }
        }
        let control = control.cloned().unwrap_or_else(|| {
            StorageReadControl::with_limit(
                crate::mvcc::VersionedSessionOptions::default().retained_bytes,
            )
        });
        Ok(Self::Sparse(Sparse {
            words: Map::new(control.memory(), control.memory().limit() / 32),
            control,
        }))
    }

    pub(super) fn insert(&mut self, id: NodeId) -> StorageBackendResult<bool> {
        match self {
            Self::Sparse(set) => {
                set.control.check()?;
                let key = u128::from(id / 64);
                let word = set.words.get(key)?.map_or(0, |word| *word);
                let mask = 1_u64 << (id % 64);
                if word & mask != 0 {
                    return Ok(false);
                }
                set.words.insert(key, word | mask, Some(&set.control))?;
                Ok(true)
            }
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
