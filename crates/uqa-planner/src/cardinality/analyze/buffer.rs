//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Mutable distribution scratch owns only its vector buffer; each element owns its payload leases.

use uqa_core::memory::{BudgetedVec, MemoryReservation, ProductionControl};
use uqa_sql::SQLError;

pub(super) enum Buffer<T> {
    Ordinary(Vec<T>),
    Controlled(BudgetedVec<T>),
}

impl<T> Buffer<T> {
    pub(super) fn new(control: ProductionControl<'_>) -> Self {
        control.budget().map_or_else(
            || Self::Ordinary(Vec::new()),
            |budget| Self::Controlled(BudgetedVec::new(budget)),
        )
    }

    pub(super) fn push(&mut self, value: T) -> Result<(), SQLError> {
        match self {
            Self::Ordinary(values) => values.push(value),
            Self::Controlled(values) => values.push(value)?,
        }
        Ok(())
    }

    pub(super) fn pop(&mut self) -> Option<T> {
        match self {
            Self::Ordinary(values) => values.pop(),
            Self::Controlled(values) => values.pop(),
        }
    }
}

impl<T> std::ops::Deref for Buffer<T> {
    type Target = [T];
    fn deref(&self) -> &[T] {
        match self {
            Self::Ordinary(values) => values,
            Self::Controlled(values) => values,
        }
    }
}

impl<T> std::ops::DerefMut for Buffer<T> {
    fn deref_mut(&mut self) -> &mut [T] {
        match self {
            Self::Ordinary(values) => values,
            Self::Controlled(values) => values,
        }
    }
}

pub(super) struct IntoIter<T> {
    values: std::vec::IntoIter<T>,
    // Drop the iterator, including any unconsumed elements and its buffer, before the buffer lease.
    _memory: Option<MemoryReservation>,
}

impl<T> Iterator for IntoIter<T> {
    type Item = T;
    fn next(&mut self) -> Option<T> {
        self.values.next()
    }
}

impl<T> IntoIterator for Buffer<T> {
    type Item = T;
    type IntoIter = IntoIter<T>;
    fn into_iter(self) -> Self::IntoIter {
        let (values, memory) = match self {
            Self::Ordinary(values) => (values, None),
            Self::Controlled(values) => {
                let (values, memory) = values.into_parts();
                (values, Some(memory))
            }
        };
        IntoIter {
            values: values.into_iter(),
            _memory: memory,
        }
    }
}
