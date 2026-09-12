//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A rolling deque whose retained capacity remains charged after prefix removal.

use std::collections::VecDeque;

use super::{replacement, MemoryBudget, MemoryError, MemoryReservation};

#[derive(Debug)]
pub struct BudgetedDeque<T> {
    values: VecDeque<T>,
    memory: MemoryReservation,
}

impl<T> BudgetedDeque<T> {
    pub fn new(budget: &MemoryBudget) -> Self {
        Self {
            values: VecDeque::new(),
            memory: budget.empty_reservation(),
        }
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    pub fn capacity(&self) -> usize {
        self.values.capacity()
    }

    pub fn reserve(&mut self, additional: usize) -> Result<(), MemoryError> {
        let required = self
            .len()
            .checked_add(additional)
            .ok_or(MemoryError::SizeOverflow)?;
        if required <= self.values.capacity() {
            return Ok(());
        }
        let (capacity, memory) = replacement::<T>(self.memory.budget(), self.capacity(), required)?;
        let mut values = VecDeque::new();
        values.try_reserve_exact(capacity)?;
        while let Some(value) = self.values.pop_front() {
            values.push_back(value);
        }
        self.values = values;
        self.memory = memory;
        Ok(())
    }

    pub fn push_back(&mut self, value: T) -> Result<(), MemoryError> {
        self.reserve(1)?;
        self.values.push_back(value);
        Ok(())
    }

    pub fn push_front(&mut self, value: T) -> Result<(), MemoryError> {
        self.reserve(1)?;
        self.values.push_front(value);
        Ok(())
    }

    pub fn pop_front(&mut self) -> Option<T> {
        self.values.pop_front()
    }

    pub fn pop_back(&mut self) -> Option<T> {
        self.values.pop_back()
    }

    pub fn iter(&self) -> std::collections::vec_deque::Iter<'_, T> {
        self.values.iter()
    }
}

impl<T> std::ops::Index<usize> for BudgetedDeque<T> {
    type Output = T;

    fn index(&self, index: usize) -> &T {
        &self.values[index]
    }
}

impl<'a, T> IntoIterator for &'a BudgetedDeque<T> {
    type Item = &'a T;
    type IntoIter = std::collections::vec_deque::Iter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<T> std::ops::IndexMut<usize> for BudgetedDeque<T> {
    fn index_mut(&mut self, index: usize) -> &mut T {
        &mut self.values[index]
    }
}
