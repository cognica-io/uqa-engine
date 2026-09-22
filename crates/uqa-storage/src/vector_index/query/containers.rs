//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::{
    collections::{BinaryHeap, HashSet},
    hash::Hash,
};

use uqa_core::memory::{BudgetedBinaryHeap, BudgetedHashSet};

use crate::{read_control::StorageReadControl, StorageBackendResult};

use super::VectorQueryBuffer;

pub(crate) enum QueryHeap<T> {
    Ordinary(BinaryHeap<T>),
    Controlled(BudgetedBinaryHeap<T>),
}

impl<T: Ord> QueryHeap<T> {
    pub(crate) fn new(control: Option<&StorageReadControl>) -> Self {
        match control {
            Some(control) => Self::Controlled(BudgetedBinaryHeap::new(control.memory())),
            None => Self::Ordinary(BinaryHeap::new()),
        }
    }

    pub(crate) fn len(&self) -> usize {
        match self {
            Self::Ordinary(heap) => heap.len(),
            Self::Controlled(heap) => heap.len(),
        }
    }

    pub(crate) fn peek(&self) -> Option<&T> {
        match self {
            Self::Ordinary(heap) => heap.peek(),
            Self::Controlled(heap) => heap.peek(),
        }
    }

    pub(crate) fn push(&mut self, value: T) -> StorageBackendResult<()> {
        match self {
            Self::Ordinary(heap) => heap.push(value),
            Self::Controlled(heap) => heap.push(value)?,
        }
        Ok(())
    }

    pub(crate) fn pop(&mut self) -> Option<T> {
        match self {
            Self::Ordinary(heap) => heap.pop(),
            Self::Controlled(heap) => heap.pop(),
        }
    }

    pub(crate) fn into_vec(self) -> VectorQueryBuffer<T> {
        match self {
            Self::Ordinary(heap) => VectorQueryBuffer::ordinary(heap.into_vec()),
            Self::Controlled(heap) => VectorQueryBuffer::controlled(heap.into_vec()),
        }
    }
}

pub(crate) enum QuerySet<T> {
    Ordinary(HashSet<T>),
    Controlled(BudgetedHashSet<T>),
}

impl<T: Eq + Hash> QuerySet<T> {
    pub(crate) fn with_capacity(
        capacity: usize,
        control: Option<&StorageReadControl>,
    ) -> StorageBackendResult<Self> {
        match control {
            Some(control) => {
                let mut set = BudgetedHashSet::new(control.memory());
                set.reserve(capacity)?;
                Ok(Self::Controlled(set))
            }
            None => Ok(Self::Ordinary(HashSet::with_capacity(capacity))),
        }
    }

    pub(crate) fn insert(&mut self, value: T) -> StorageBackendResult<bool> {
        match self {
            Self::Ordinary(set) => Ok(set.insert(value)),
            Self::Controlled(set) => Ok(set.insert(value)?),
        }
    }
}
