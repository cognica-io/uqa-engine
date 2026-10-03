//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Search queues preserve total candidate ordering when their heaps move to temporary pages.

use std::cmp::Ordering;
use uqa_core::memory::{BudgetedBinaryHeap, BudgetedVec, MemoryError};

use super::{
    search::Candidate,
    store::{Iter, Map},
};
use crate::{read_control::StorageReadControl, StorageBackendResult};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct Priority<const MIN: bool>(Candidate);

impl<const MIN: bool> Ord for Priority<MIN> {
    fn cmp(&self, other: &Self) -> Ordering {
        let order = self.0.cmp(&other.0);
        if MIN {
            order.reverse()
        } else {
            order
        }
    }
}

impl<const MIN: bool> PartialOrd for Priority<MIN> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

enum Root<const MIN: bool> {
    Heap(BudgetedBinaryHeap<Priority<MIN>>),
    Pages(Map<u64>),
}

pub(super) struct Queue<const MIN: bool> {
    root: Root<MIN>,
    control: StorageReadControl,
}

impl<const MIN: bool> Queue<MIN> {
    pub(super) fn new(control: &StorageReadControl) -> Self {
        Self {
            root: Root::Heap(BudgetedBinaryHeap::new(
                &control.memory().child(control.memory().limit() / 32),
            )),
            control: control.clone(),
        }
    }

    pub(super) fn len(&self) -> usize {
        match &self.root {
            Root::Heap(heap) => heap.len(),
            Root::Pages(map) => map.len(),
        }
    }

    pub(super) fn peek(&self) -> StorageBackendResult<Option<Candidate>> {
        self.control.check()?;
        match &self.root {
            Root::Heap(heap) => Ok(heap.peek().map(|value| value.0)),
            Root::Pages(map) => {
                Ok(if MIN { map.next(None)? } else { map.last()? }.map(|(key, _)| decode(key)))
            }
        }
    }

    pub(super) fn pop(&mut self) -> StorageBackendResult<Option<Candidate>> {
        self.control.check()?;
        match &mut self.root {
            Root::Heap(heap) => Ok(heap.pop().map(|value| value.0)),
            Root::Pages(map) => {
                let first = if MIN { map.next(None)? } else { map.last()? }.map(|(key, _)| key);
                if let Some(key) = first {
                    map.remove(key, Some(&self.control))?;
                }
                Ok(first.map(decode))
            }
        }
    }

    pub(super) fn push(&mut self, candidate: Candidate) -> StorageBackendResult<()> {
        self.control.check()?;
        if let Root::Heap(heap) = &mut self.root {
            match heap.push(Priority(candidate)) {
                Ok(()) => return Ok(()),
                Err(MemoryError::Limit { .. }) => (),
                Err(error) => return Err(error.into()),
            }
            let mut pages = Map::new(self.control.memory(), 0);
            // Retain the old heap until the complete replacement exists. A failed spill leaves its candidates intact.
            for value in heap.as_slice() {
                pages.insert(encode(value.0), 0, Some(&self.control))?;
            }
            self.root = Root::Pages(pages);
        }
        if let Root::Pages(map) = &mut self.root {
            map.insert(encode(candidate), 0, Some(&self.control))?;
        }
        Ok(())
    }
}

impl Queue<false> {
    pub(super) fn into_sorted(self) -> Candidates {
        match self.root {
            Root::Heap(heap) => {
                let mut values = heap.into_vec();
                values.sort_unstable();
                Candidates::Memory(values)
            }
            Root::Pages(map) => Candidates::Pages(map),
        }
    }
}

pub(super) enum Candidates {
    Memory(BudgetedVec<Priority<false>>),
    Pages(Map<u64>),
}

impl Candidates {
    pub(super) fn iter(&self) -> CandidateIter<'_> {
        match self {
            Self::Memory(values) => CandidateIter::Memory(values.iter()),
            Self::Pages(map) => CandidateIter::Pages(map.iter()),
        }
    }
}

pub(super) enum CandidateIter<'a> {
    Memory(std::slice::Iter<'a, Priority<false>>),
    Pages(Iter<'a, u64>),
}

impl Iterator for CandidateIter<'_> {
    type Item = StorageBackendResult<Candidate>;
    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Memory(values) => values.next().map(|value| Ok(value.0)),
            Self::Pages(values) => values.next().map(|entry| entry.map(|(key, _)| decode(key))),
        }
    }
}

fn encode(candidate: Candidate) -> u128 {
    let bits = candidate.distance.to_bits();
    let ordered = if bits & (1 << 31) != 0 {
        !bits
    } else {
        bits ^ (1 << 31)
    };
    (u128::from(ordered) << 64) | u128::from(candidate.node_id)
}

fn decode(key: u128) -> Candidate {
    let ordered = (key >> 64) as u32;
    let bits = if ordered & (1 << 31) != 0 {
        ordered ^ (1 << 31)
    } else {
        !ordered
    };
    Candidate {
        distance: f32::from_bits(bits),
        node_id: key as u64,
    }
}

#[cfg(test)]
mod tests;
