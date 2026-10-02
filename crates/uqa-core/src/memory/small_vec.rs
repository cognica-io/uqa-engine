//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Fallible vectors whose first elements live inline. Only a heap buffer is charged, and both buffers stay charged while elements move between them.

use smallvec::{Array, SmallVec};

use super::{reconcile_buffer_capacity, replacement, MemoryBudget, MemoryError, MemoryReservation};

pub struct BudgetedSmallVec<A: Array> {
    values: SmallVec<A>,
    memory: MemoryReservation,
}

impl<A: Array> BudgetedSmallVec<A> {
    pub fn new(budget: &MemoryBudget) -> Self {
        Self {
            values: SmallVec::new(),
            memory: budget.empty_reservation(),
        }
    }

    pub fn capacity(&self) -> usize {
        self.values.capacity()
    }

    /// Whether the elements moved to a charged heap buffer.
    pub fn spilled(&self) -> bool {
        self.values.spilled()
    }

    pub fn budget(&self) -> &MemoryBudget {
        self.memory.budget()
    }

    /// Charge a heap buffer for `additional` more elements before allocating it. A failed reservation or allocation preserves the original elements, buffer and reservation.
    pub fn reserve(&mut self, additional: usize) -> Result<(), MemoryError> {
        let required = self
            .values
            .len()
            .checked_add(additional)
            .ok_or(MemoryError::SizeOverflow)?;
        if required <= self.values.capacity() {
            return Ok(());
        }
        let heap = if self.values.spilled() {
            self.values.capacity()
        } else {
            0
        };
        let (capacity, mut memory) = replacement::<A::Item>(self.memory.budget(), heap, required)?;
        let mut buffer = Vec::new();
        buffer.try_reserve_exact(capacity)?;
        reconcile_buffer_capacity::<A::Item>(&mut memory, buffer.capacity())?;
        buffer.extend(self.values.drain(..));
        // The required capacity exceeds the inline capacity, so the buffer stays on the heap. Field order frees the old buffer before releasing its reservation.
        self.values = SmallVec::from_vec(buffer);
        self.memory = memory;
        Ok(())
    }

    pub fn push(&mut self, value: A::Item) -> Result<(), MemoryError> {
        self.reserve(1)?;
        self.values.push(value);
        Ok(())
    }

    pub fn pop(&mut self) -> Option<A::Item> {
        self.values.pop()
    }

    pub fn clear(&mut self) {
        self.values.clear();
    }

    pub fn truncate(&mut self, len: usize) {
        self.values.truncate(len);
    }
}

impl<A: Array> std::ops::Deref for BudgetedSmallVec<A> {
    type Target = [A::Item];

    fn deref(&self) -> &[A::Item] {
        &self.values
    }
}

impl<A: Array> std::ops::DerefMut for BudgetedSmallVec<A> {
    fn deref_mut(&mut self) -> &mut [A::Item] {
        &mut self.values
    }
}

impl<A: Array> std::fmt::Debug for BudgetedSmallVec<A>
where
    A::Item: std::fmt::Debug,
{
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BudgetedSmallVec")
            .field("values", &&*self.values)
            .field("memory", &self.memory)
            .finish()
    }
}

#[cfg(test)]
mod tests;
