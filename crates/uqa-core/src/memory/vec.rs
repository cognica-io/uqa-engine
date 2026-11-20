//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Fallible vectors charge both buffers while moving between allocations.

use super::{
    buffer_bytes, reconcile_buffer_capacity, replacement, MemoryBudget, MemoryError,
    MemoryReservation,
};

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

    /// Rejoin a buffer and its original reservation without allocating or reacquiring its allowance. The reservation must cover exactly the buffer capacity; element payloads keep their separate owners.
    pub fn from_parts(values: Vec<T>, memory: MemoryReservation) -> Self {
        let output = Self { values, memory };
        assert_eq!(
            output.memory.bytes(),
            buffer_bytes::<T>(output.values.capacity()).expect("existing vector capacity"),
            "vector buffer and reservation must match"
        );
        output
    }

    pub fn capacity(&self) -> usize {
        self.values.capacity()
    }

    pub fn budget(&self) -> &MemoryBudget {
        self.memory.budget()
    }

    pub fn reserve(&mut self, additional: usize) -> Result<(), MemoryError> {
        if let Some(buffer) = self.growth_buffer(additional)? {
            self.replace_buffer(buffer)?;
        }
        Ok(())
    }

    fn growth_buffer(&self, additional: usize) -> Result<Option<Self>, MemoryError> {
        let required = self
            .values
            .len()
            .checked_add(additional)
            .ok_or(MemoryError::SizeOverflow)?;
        if required <= self.capacity() {
            return Ok(None);
        }
        let (capacity, memory) = replacement::<T>(self.budget(), self.capacity(), required)?;
        Self::allocate_buffer(capacity, memory).map(Some)
    }

    fn allocate_buffer(capacity: usize, memory: MemoryReservation) -> Result<Self, MemoryError> {
        let mut values = Vec::new();
        values.try_reserve_exact(capacity)?;
        Ok(Self { values, memory })
    }

    fn reconciled(mut self) -> Result<Self, MemoryError> {
        reconcile_buffer_capacity::<T>(&mut self.memory, self.values.capacity())?;
        Ok(self)
    }

    fn replace_buffer(&mut self, buffer: Self) -> Result<(), MemoryError> {
        let mut buffer = buffer.reconciled()?;
        buffer.values.append(&mut self.values);
        // Field order frees the old buffer before releasing its reservation.
        *self = buffer;
        Ok(())
    }

    pub(super) fn push_needs_work(&self) -> bool {
        self.values.len() == self.capacity() && self.values.len() > relocation_slots::<T>()
    }

    pub(super) fn relocate_for_push(self) -> Result<Relocation<T>, MemoryError> {
        let destination = self
            .growth_buffer(1)?
            .expect("full vector selected for relocation")
            .reconciled()?;
        let (values, memory) = self.into_parts();
        Ok(Relocation {
            source: values.into_iter(),
            destination,
            _source_memory: memory,
        })
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

    /// Remove consecutive duplicate elements without changing the admitted buffer capacity.
    pub fn dedup_by_key<K: PartialEq>(&mut self, key: impl FnMut(&mut T) -> K) {
        self.values.dedup_by_key(key);
    }

    /// Request a buffer for the current length, charging the reported capacity of both buffers until the move finishes. A failed reservation or allocation preserves the original values, buffer and reservation.
    pub fn shrink_to_fit(&mut self) -> Result<(), MemoryError> {
        let capacity = self.values.len();
        if capacity == self.values.capacity() {
            return Ok(());
        }
        let memory = self.memory.budget().reserve(buffer_bytes::<T>(capacity)?)?;
        self.replace_buffer(Self::allocate_buffer(capacity, memory)?)
    }

    pub fn pop(&mut self) -> Option<T> {
        self.values.pop()
    }

    /// Transfer the buffer and its reservation together; element allocations have separate owners.
    pub fn into_parts(self) -> (Vec<T>, MemoryReservation) {
        (self.values, self.memory)
    }
}

fn relocation_slots<T>() -> usize {
    (4096 / size_of::<T>().max(1)).max(1)
}

/// Both buffers stay charged while their slots move. Dropping unfinished construction drops each element once.
pub(super) struct Relocation<T> {
    source: std::vec::IntoIter<T>,
    destination: BudgetedVec<T>,
    _source_memory: MemoryReservation,
}

impl<T> Relocation<T> {
    pub(super) fn remaining(&self) -> usize {
        self.source.len()
    }

    pub(super) fn advance(&mut self) {
        self.destination
            .values
            .extend(self.source.by_ref().take(relocation_slots::<T>()));
    }

    pub(super) fn finish(self) -> BudgetedVec<T> {
        assert_eq!(self.remaining(), 0, "all vector slots transferred");
        self.destination
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

impl BudgetedVec<crate::Value> {
    pub(crate) fn push_owned_value(
        &mut self,
        value: crate::types::OwnedValue,
    ) -> Result<(), MemoryError> {
        self.reserve(1)?;
        self.values.push(value.into_inner());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn large_vector_growth_moves_ordered_slots_in_charged_byte_windows() {
        type Slot = [usize; 10];
        let empty_budget = MemoryBudget::new(0);
        let mut empty = BudgetedVec::new(&empty_budget);
        for _ in 0..4096 {
            empty.push(()).unwrap();
        }
        assert!(!empty.push_needs_work());
        assert_eq!(empty_budget.used(), 0);

        let count = 4_096;
        let bytes = count * size_of::<Slot>();
        let budget = MemoryBudget::new(3 * bytes);
        let mut values = BudgetedVec::new(&budget);
        values.reserve(count).unwrap();
        for index in 0..count {
            values.push([index; 10]).unwrap();
        }
        assert!(values.push_needs_work());
        let mut moving = values.relocate_for_push().unwrap();
        assert_eq!(moving.remaining(), count);
        assert_eq!(budget.used(), 3 * bytes);
        let mut steps = 0;
        while moving.remaining() != 0 {
            let before = moving.remaining();
            let measured = allocation_counter::measure(|| moving.advance());
            let moved = (before - moving.remaining()) * size_of::<Slot>();
            assert!(moved > 0 && moved <= 4096);
            assert_eq!(measured.count_total, 0);
            assert_eq!(measured.count_current, 0);
            steps += 1;
        }
        assert_eq!(steps, count.div_ceil(4096 / size_of::<Slot>()));
        let result = moving.finish();
        assert_eq!(budget.used(), result.capacity() * size_of::<Slot>());
        assert!(result.iter().enumerate().all(|(i, slot)| *slot == [i; 10]));
        drop(result);
        assert_eq!(budget.used(), 0);
    }

    #[test]
    fn rejoining_vector_parts_preserves_admission_and_original_allocation() {
        let budget = MemoryBudget::new(32);
        let mut source = BudgetedVec::new(&budget);
        source.extend_from_slice(&[1_u32, 2, 3, 4]).unwrap();
        let address = source.as_ptr();
        let (values, memory) = source.into_parts();
        let occupied = budget.reserve(budget.available()).unwrap();
        let mut restored = None;
        let allocation = allocation_counter::measure(|| {
            restored = Some(BudgetedVec::from_parts(values, memory));
        });
        let mut restored = restored.unwrap();
        assert_eq!(allocation.count_total, 0);
        assert_eq!(restored.as_ptr(), address);
        assert!(restored.push(5).is_err());
        assert_eq!(&*restored, &[1, 2, 3, 4]);
        drop(occupied);
        drop(restored);
        assert_eq!(budget.used(), 0);

        let values = Vec::<u32>::with_capacity(4);
        let memory = budget.reserve(4).unwrap();
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            BudgetedVec::from_parts(values, memory)
        }))
        .is_err());
        assert_eq!(budget.used(), 0);
    }

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
    fn oversized_replacement_is_charged_before_moving_the_original_values() {
        for reject in [false, true] {
            let mut original = vec![11_u32, 22];
            original.reserve_exact(6);
            let replacement = Vec::with_capacity(12);
            let original_capacity = original.capacity();
            let original_bytes = original_capacity * size_of::<u32>();
            let replacement_bytes = replacement.capacity() * size_of::<u32>();
            let budget =
                MemoryBudget::new(original_bytes + replacement_bytes - usize::from(reject));
            let mut values = BudgetedVec {
                values: original,
                memory: budget.reserve(original_bytes).unwrap(),
            };
            // Model an allocator reporting more capacity than the requested length.
            let buffer = BudgetedVec {
                values: replacement,
                memory: budget.reserve(values.len() * size_of::<u32>()).unwrap(),
            };
            let result = values.replace_buffer(buffer);
            assert_eq!(&*values, &[11, 22]);
            if reject {
                assert!(matches!(result, Err(MemoryError::Limit { .. })));
                assert_eq!(values.capacity(), original_capacity);
                assert_eq!(budget.used(), original_bytes);
            } else {
                result.unwrap();
                assert_eq!(budget.used(), replacement_bytes);
                assert_eq!(budget.peak(), original_bytes + replacement_bytes);
            }
            drop(values);
            assert_eq!(budget.used(), 0);
        }
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
