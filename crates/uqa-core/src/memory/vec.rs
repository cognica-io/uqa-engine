//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Fallible vectors charge both buffers while moving to a larger allocation.

use super::{replacement, MemoryBudget, MemoryError, MemoryReservation};

#[derive(Debug)]
pub struct BudgetedVec<T> {
    values: Vec<T>,
    memory: MemoryReservation,
}

impl<T> BudgetedVec<T> {
    pub fn new(budget: &MemoryBudget) -> Self {
        Self {
            values: Vec::new(),
            memory: budget.empty_reservation(),
        }
    }

    pub fn capacity(&self) -> usize {
        self.values.capacity()
    }

    pub fn budget(&self) -> &MemoryBudget {
        self.memory.budget()
    }

    pub fn reserve(&mut self, additional: usize) -> Result<(), MemoryError> {
        let required = self
            .values
            .len()
            .checked_add(additional)
            .ok_or(MemoryError::SizeOverflow)?;
        if required <= self.values.capacity() {
            return Ok(());
        }
        let (capacity, memory) = replacement::<T>(self.memory.budget(), self.capacity(), required)?;
        let mut values = Vec::new();
        values.try_reserve_exact(capacity)?;
        values.append(&mut self.values);
        // Free the old buffer before releasing its reservation.
        self.values = values;
        self.memory = memory;
        Ok(())
    }

    pub fn push(&mut self, value: T) -> Result<(), MemoryError> {
        self.reserve(1)?;
        self.values.push(value);
        Ok(())
    }

    pub fn clear(&mut self) {
        self.values.clear();
    }

    pub fn truncate(&mut self, len: usize) {
        self.values.truncate(len);
    }

    pub fn pop(&mut self) -> Option<T> {
        self.values.pop()
    }

    /// Transfer the buffer and its reservation together; element allocations have separate owners.
    pub fn into_parts(self) -> (Vec<T>, MemoryReservation) {
        (self.values, self.memory)
    }
}

impl<T> std::ops::Deref for BudgetedVec<T> {
    type Target = [T];

    fn deref(&self) -> &[T] {
        &self.values
    }
}

impl<T> std::ops::DerefMut for BudgetedVec<T> {
    fn deref_mut(&mut self) -> &mut [T] {
        &mut self.values
    }
}
