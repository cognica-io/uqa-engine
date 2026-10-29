//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Mutable SQL scratch keeps buffer admission separate from independently owned element payloads.

use crate::SQLError;
use uqa_core::memory::{BudgetedVec, MemoryReservation, ProductionControl};

pub(crate) enum Buffer<T> {
    Ordinary(Vec<T>),
    Controlled(BudgetedVec<T>),
}

impl<T> Buffer<T> {
    pub(crate) fn new(control: &ProductionControl<'_>) -> Self {
        control.budget().map_or_else(
            || Self::Ordinary(Vec::new()),
            |budget| Self::Controlled(BudgetedVec::new(budget)),
        )
    }

    pub(crate) fn push(
        &mut self,
        value: T,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        control.check()?;
        match self {
            Self::Ordinary(values) => values.push(value),
            Self::Controlled(values) => values.push(value)?,
        }
        Ok(())
    }

    pub(crate) fn pop(&mut self) -> Option<T> {
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

pub(crate) struct IntoIter<T> {
    values: std::vec::IntoIter<T>,
    _memory: Option<MemoryReservation>,
}

impl<T> Iterator for IntoIter<T> {
    type Item = T;
    fn next(&mut self) -> Option<T> {
        self.values.next()
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.values.size_hint()
    }
}
impl<T> ExactSizeIterator for IntoIter<T> {}

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
