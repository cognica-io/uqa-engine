//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use uqa_core::memory::{BudgetedVec, MemoryReservation};

use crate::{read_control::StorageReadControl, StorageBackendResult};

enum Buffer<T> {
    Ordinary(Vec<T>),
    Controlled(BudgetedVec<T>),
}

/// Owned query output whose allocation stays charged to the selected reader when a read control is supplied. Callers may borrow the values without separating them from their lease; ordinary uncontrolled index APIs retain their existing caller-owned allocation boundary.
pub struct VectorQueryBuffer<T>(Buffer<T>);

impl<T> VectorQueryBuffer<T> {
    pub(crate) fn new(control: Option<&StorageReadControl>) -> Self {
        Self(match control {
            Some(control) => Buffer::Controlled(BudgetedVec::new(control.memory())),
            None => Buffer::Ordinary(Vec::new()),
        })
    }

    pub(crate) fn ordinary(values: Vec<T>) -> Self {
        Self(Buffer::Ordinary(values))
    }

    pub(crate) fn controlled(values: BudgetedVec<T>) -> Self {
        Self(Buffer::Controlled(values))
    }

    pub(crate) fn reserve(&mut self, additional: usize) -> StorageBackendResult<()> {
        match &mut self.0 {
            Buffer::Ordinary(values) => values.reserve(additional),
            Buffer::Controlled(values) => values.reserve(additional)?,
        }
        Ok(())
    }

    pub(crate) fn push(&mut self, value: T) -> StorageBackendResult<()> {
        match &mut self.0 {
            Buffer::Ordinary(values) => values.push(value),
            Buffer::Controlled(values) => values.push(value)?,
        }
        Ok(())
    }

    pub(crate) fn clear(&mut self) {
        match &mut self.0 {
            Buffer::Ordinary(values) => values.clear(),
            Buffer::Controlled(values) => values.clear(),
        }
    }

    pub(crate) fn truncate(&mut self, len: usize) {
        match &mut self.0 {
            Buffer::Ordinary(values) => values.truncate(len),
            Buffer::Controlled(values) => values.truncate(len),
        }
    }

    pub(crate) fn into_parts(self) -> (Vec<T>, Option<MemoryReservation>) {
        match self.0 {
            Buffer::Ordinary(values) => (values, None),
            Buffer::Controlled(values) => {
                let (values, memory) = values.into_parts();
                (values, Some(memory))
            }
        }
    }
}

impl<T: Copy> VectorQueryBuffer<T> {
    pub(crate) fn extend_from_slice(&mut self, source: &[T]) -> StorageBackendResult<()> {
        match &mut self.0 {
            Buffer::Ordinary(values) => values.extend_from_slice(source),
            Buffer::Controlled(values) => values.extend_from_slice(source)?,
        }
        Ok(())
    }
}

impl<T> std::ops::Deref for VectorQueryBuffer<T> {
    type Target = [T];

    fn deref(&self) -> &[T] {
        match &self.0 {
            Buffer::Ordinary(values) => values,
            Buffer::Controlled(values) => values,
        }
    }
}

impl<T> std::ops::DerefMut for VectorQueryBuffer<T> {
    fn deref_mut(&mut self) -> &mut [T] {
        match &mut self.0 {
            Buffer::Ordinary(values) => values,
            Buffer::Controlled(values) => values,
        }
    }
}
