//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Fallible vectors charge both buffers while moving between allocations.

use super::{buffer_bytes, replacement, MemoryBudget, MemoryError, MemoryReservation};

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

    /// Replace excess capacity with an exact-sized buffer, charging both buffers until the move finishes. A failed reservation or allocation preserves the original values, buffer and reservation.
    pub fn shrink_to_fit(&mut self) -> Result<(), MemoryError> {
        let capacity = self.values.len();
        if capacity == self.values.capacity() {
            return Ok(());
        }
        let memory = self.memory.budget().reserve(buffer_bytes::<T>(capacity)?)?;
        let mut values = Vec::new();
        values.try_reserve_exact(capacity)?;
        values.append(&mut self.values);
        self.values = values;
        self.memory = memory;
        Ok(())
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

impl<T: Copy> BudgetedVec<T> {
    /// Reserve the complete destination before copying a slice into this buffer.
    pub fn extend_from_slice(&mut self, values: &[T]) -> Result<(), MemoryError> {
        self.reserve(values.len())?;
        self.values.extend_from_slice(values);
        Ok(())
    }
}

impl<T> std::ops::DerefMut for BudgetedVec<T> {
    fn deref_mut(&mut self) -> &mut [T] {
        &mut self.values
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extending_a_slice_reserves_before_mutating_and_preserves_failed_inputs() {
        let budget = MemoryBudget::new(16);
        let mut values = BudgetedVec::new(&budget);
        values.extend_from_slice(&[1_u8, 2, 3, 4]).unwrap();
        let retained = budget.used();
        assert!(matches!(
            values.extend_from_slice(&[9; 32]),
            Err(MemoryError::Limit { .. })
        ));
        assert_eq!(&*values, &[1, 2, 3, 4]);
        assert_eq!(budget.used(), retained);
        values.extend_from_slice(&[5, 6]).unwrap();
        assert_eq!(&*values, &[1, 2, 3, 4, 5, 6]);
        assert_eq!(budget.used(), values.capacity());
        drop(values);
        assert_eq!(budget.used(), 0);
    }

    #[test]
    fn shrinking_releases_excess_capacity_after_charging_both_buffers() {
        let budget = MemoryBudget::new(32);
        let mut values = BudgetedVec::new(&budget);
        values
            .extend_from_slice(&[1_u8, 2, 3, 4, 5, 6, 7, 8])
            .unwrap();
        values.truncate(2);
        values.shrink_to_fit().unwrap();
        assert_eq!(&*values, &[1, 2]);
        assert_eq!(values.capacity(), 2);
        assert_eq!(budget.used(), 2);
        assert_eq!(budget.peak(), 10);
        values.shrink_to_fit().unwrap();
        assert_eq!(budget.used(), 2);
    }

    #[test]
    fn failed_shrinking_preserves_the_buffer_and_empty_shrinking_needs_no_headroom() {
        let budget = MemoryBudget::new(8);
        let mut values = BudgetedVec::new(&budget);
        values
            .extend_from_slice(&[1_u8, 2, 3, 4, 5, 6, 7, 8])
            .unwrap();
        values.truncate(2);
        assert!(matches!(
            values.shrink_to_fit(),
            Err(MemoryError::Limit { .. })
        ));
        assert_eq!(&*values, &[1, 2]);
        assert_eq!(values.capacity(), 8);
        assert_eq!(budget.used(), 8);
        values.clear();
        values.shrink_to_fit().unwrap();
        assert_eq!(values.capacity(), 0);
        assert_eq!(budget.used(), 0);
    }

    #[test]
    fn shrinking_moves_owned_values_without_copying_or_dropping_survivors() {
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };
        struct Item(Arc<AtomicUsize>);
        impl Drop for Item {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::Relaxed);
            }
        }
        let budget = MemoryBudget::new(1024);
        let dropped = Arc::new(AtomicUsize::new(0));
        let mut values = BudgetedVec::new(&budget);
        for _ in 0..4 {
            values.push(Item(Arc::clone(&dropped))).unwrap();
        }
        values.truncate(1);
        assert_eq!(dropped.load(Ordering::Relaxed), 3);
        values.shrink_to_fit().unwrap();
        assert_eq!(dropped.load(Ordering::Relaxed), 3);
        drop(values);
        assert_eq!(dropped.load(Ordering::Relaxed), 4);
        assert_eq!(budget.used(), 0);
    }
}
