//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Analysis scratch buffers admit capacity before moving independently owned elements.

use crate::SQLError;
use uqa_core::memory::{BudgetedVec, MemoryReservation, Produced, ProductionControl};
mod types;
pub(super) use types::AnalysisType;
mod maps;
pub(in crate::binding) use maps::AnalysisMap;

pub(super) enum AnalysisVec<T> {
    Ordinary(Vec<T>),
    Controlled(BudgetedVec<T>),
}

impl<T> AnalysisVec<T> {
    pub(super) fn new(control: &ProductionControl<'_>) -> Self {
        control.budget().map_or_else(
            || Self::Ordinary(Vec::new()),
            |budget| Self::Controlled(BudgetedVec::new(budget)),
        )
    }

    pub(super) fn reserve(
        &mut self,
        additional: usize,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        control.check()?;
        match self {
            Self::Ordinary(values) => {
                assert!(control.budget().is_none());
                values
                    .try_reserve(additional)
                    .map_err(uqa_core::memory::MemoryError::from)?;
            }
            Self::Controlled(values) => {
                assert!(control
                    .budget()
                    .is_some_and(|budget| budget.shares_allowance(values.budget())));
                values.reserve(additional)?;
            }
        }
        Ok(())
    }

    pub(super) fn push(
        &mut self,
        value: T,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        self.reserve(1, control)?;
        match self {
            Self::Ordinary(values) => values.push(value),
            Self::Controlled(values) => values.push(value)?,
        }
        Ok(())
    }

    pub(super) fn clear(&mut self) {
        self.truncate(0);
    }

    pub(super) fn truncate(&mut self, len: usize) {
        match self {
            Self::Ordinary(values) => values.truncate(len),
            Self::Controlled(values) => values.truncate(len),
        }
    }

    pub(super) fn into_parts(self) -> (Vec<T>, Option<MemoryReservation>) {
        match self {
            Self::Ordinary(values) => (values, None),
            Self::Controlled(values) => {
                let (values, memory) = values.into_parts();
                (values, Some(memory))
            }
        }
    }
}

impl<T> std::ops::Deref for AnalysisVec<T> {
    type Target = [T];
    fn deref(&self) -> &[T] {
        match self {
            Self::Ordinary(values) => values,
            Self::Controlled(values) => values,
        }
    }
}

impl<T> std::ops::DerefMut for AnalysisVec<T> {
    fn deref_mut(&mut self) -> &mut [T] {
        match self {
            Self::Ordinary(values) => values,
            Self::Controlled(values) => values,
        }
    }
}

impl<'a, T> IntoIterator for &'a AnalysisVec<T> {
    type Item = &'a T;
    type IntoIter = std::slice::Iter<'a, T>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<'a, T> IntoIterator for &'a mut AnalysisVec<T> {
    type Item = &'a mut T;
    type IntoIter = std::slice::IterMut<'a, T>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter_mut()
    }
}

pub(super) struct AnalysisItems<T> {
    items: std::vec::IntoIter<T>,
    // The iterator owns the allocation until it is exhausted or dropped.
    _memory: Option<MemoryReservation>,
}

impl<T> IntoIterator for AnalysisVec<T> {
    type Item = T;
    type IntoIter = AnalysisItems<T>;
    fn into_iter(self) -> Self::IntoIter {
        let (items, memory) = self.into_parts();
        AnalysisItems {
            items: items.into_iter(),
            _memory: memory,
        }
    }
}

impl<T> AnalysisItems<T> {
    pub(super) fn from_produced(values: Produced<Vec<T>>) -> Self {
        let (items, memory) = values.into_parts();
        Self {
            items: items.into_iter(),
            _memory: memory,
        }
    }
}

impl<T> Iterator for AnalysisItems<T> {
    type Item = T;
    fn next(&mut self) -> Option<T> {
        self.items.next()
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.items.size_hint()
    }
}

impl<T> ExactSizeIterator for AnalysisItems<T> {}
impl<T> std::iter::FusedIterator for AnalysisItems<T> {}
