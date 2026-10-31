//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Call-state buffers retain capacity independently of immutable scalar payloads.

use uqa_core::memory::{BudgetedVec, ProductionControl};
use uqa_sql::SQLError;

#[derive(Debug)]
pub(super) enum StateEntries<T> {
    Ordinary(Vec<T>),
    Controlled(BudgetedVec<T>),
}

impl<T> Default for StateEntries<T> {
    fn default() -> Self {
        Self::Ordinary(Vec::new())
    }
}

impl<T> StateEntries<T> {
    pub(super) fn new(control: &ProductionControl<'_>) -> Self {
        control.budget().map_or_else(Self::default, |budget| {
            Self::Controlled(BudgetedVec::new(budget))
        })
    }
    pub(super) fn into_parts(self) -> (Vec<T>, Option<uqa_core::memory::MemoryReservation>) {
        match self {
            Self::Ordinary(values) => (values, None),
            Self::Controlled(values) => {
                let (values, memory) = values.into_parts();
                (values, Some(memory))
            }
        }
    }
    pub(super) fn push(&mut self, value: T) -> Result<(), SQLError> {
        match self {
            Self::Ordinary(values) => values.push(value),
            Self::Controlled(values) => values.push(value)?,
        }
        Ok(())
    }
    pub(super) fn truncate(&mut self, len: usize) {
        match self {
            Self::Ordinary(values) => values.truncate(len),
            Self::Controlled(values) => values.truncate(len),
        }
    }
    pub(super) fn dedup_by_key<K: PartialEq>(&mut self, key: impl FnMut(&mut T) -> K) {
        match self {
            Self::Ordinary(values) => values.dedup_by_key(key),
            Self::Controlled(values) => values.dedup_by_key(key),
        }
    }
}
impl<T> std::ops::Deref for StateEntries<T> {
    type Target = [T];
    fn deref(&self) -> &Self::Target {
        match self {
            Self::Ordinary(values) => values,
            Self::Controlled(values) => values,
        }
    }
}
impl<T> std::ops::DerefMut for StateEntries<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        match self {
            Self::Ordinary(values) => values,
            Self::Controlled(values) => values,
        }
    }
}
