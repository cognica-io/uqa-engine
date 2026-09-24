//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! UTF-8 buffers reserve old and replacement allocations before growing.

use super::{reconcile_buffer_capacity, replacement, MemoryBudget, MemoryError, MemoryReservation};

#[derive(Debug)]
pub struct BudgetedString {
    value: String,
    memory: MemoryReservation,
}

impl BudgetedString {
    pub fn new(budget: &MemoryBudget) -> Self {
        Self {
            value: String::new(),
            memory: budget.empty_reservation(),
        }
    }

    /// Transfer a completed string and its existing allocation lease without copying its buffer.
    pub fn from_budgeted(value: super::Budgeted<String>) -> Self {
        let (value, memory) = value.into_parts();
        Self { value, memory }
    }

    pub fn capacity(&self) -> usize {
        self.value.capacity()
    }

    pub fn reserve(&mut self, additional: usize) -> Result<(), MemoryError> {
        let required = self
            .value
            .len()
            .checked_add(additional)
            .ok_or(MemoryError::SizeOverflow)?;
        if required <= self.capacity() {
            return Ok(());
        }
        let (capacity, mut memory) =
            replacement::<u8>(self.memory.budget(), self.capacity(), required)?;
        let mut value = String::new();
        value.try_reserve_exact(capacity)?;
        reconcile_buffer_capacity::<u8>(&mut memory, value.capacity())?;
        value.push_str(&self.value);
        self.value = value;
        self.memory = memory;
        Ok(())
    }

    pub fn push_str(&mut self, text: &str) -> Result<(), MemoryError> {
        self.reserve(text.len())?;
        self.value.push_str(text);
        Ok(())
    }

    pub fn push(&mut self, character: char) -> Result<(), MemoryError> {
        self.reserve(character.len_utf8())?;
        self.value.push(character);
        Ok(())
    }

    pub fn truncate(&mut self, length: usize) {
        self.value.truncate(length);
    }

    pub fn into_parts(self) -> (String, MemoryReservation) {
        (self.value, self.memory)
    }
}

impl std::ops::Deref for BudgetedString {
    type Target = str;

    fn deref(&self) -> &str {
        &self.value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completed_string_transfer_preserves_buffer_and_lease() {
        let budget = MemoryBudget::new(128);
        let mut source = BudgetedString::new(&budget);
        source.push_str("transferred").unwrap();
        let (value, memory) = source.into_parts();
        let pointer = value.as_ptr();
        let capacity = value.capacity();
        let output = BudgetedString::from_budgeted(super::super::Budgeted::new(value, memory));
        assert_eq!(output.as_ptr(), pointer);
        assert_eq!(budget.used(), capacity);
        drop(output);
        assert_eq!(budget.used(), 0);
    }
}
