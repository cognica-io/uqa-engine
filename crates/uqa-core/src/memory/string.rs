//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! UTF-8 buffers reserve old and replacement allocations before growing.

use super::{replacement, MemoryBudget, MemoryError, MemoryReservation};

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
        let (capacity, memory) =
            replacement::<u8>(self.memory.budget(), self.capacity(), required)?;
        let mut value = String::new();
        value.try_reserve_exact(capacity)?;
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
