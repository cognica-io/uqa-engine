//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Encoded posting keys retain their buffers through query preparation and provider reads.

use super::TokenTermKey;
use uqa_analysis::TokenTerm;
use uqa_core::memory::{Budgeted, BudgetedVec, MemoryBudget, MemoryError};

impl TokenTermKey {
    /// Encode borrowed scalar text with a reserved complete key buffer and bounded callback checks.
    pub fn from_text_budgeted<E: From<MemoryError>>(
        text: &str,
        budget: &MemoryBudget,
        mut poll: impl FnMut() -> Result<(), E>,
    ) -> Result<Budgeted<Self>, E> {
        poll()?;
        let mut bytes = BudgetedVec::new(budget);
        bytes.reserve(text.len().checked_add(1).ok_or(MemoryError::SizeOverflow)?)?;
        bytes.push(0)?;
        for (index, byte) in text.bytes().enumerate() {
            if index % 1024 == 0 {
                poll()?;
            }
            bytes.push(byte)?;
        }
        poll()?;
        let (bytes, memory) = bytes.into_parts();
        Ok(Budgeted::new(Self(bytes), memory))
    }

    /// Preserve canonical scalar/raw identity while reserving the key alongside its source term.
    pub fn from_term_budgeted<E: From<MemoryError>>(
        term: &TokenTerm,
        budget: &MemoryBudget,
        mut poll: impl FnMut() -> Result<(), E>,
    ) -> Result<Budgeted<Self>, E> {
        if let Some(text) = term.as_str() {
            return Self::from_text_budgeted(text, budget, poll);
        }
        poll()?;
        let units = term.utf16();
        let mut bytes = BudgetedVec::new(budget);
        bytes.reserve(
            units
                .len()
                .checked_mul(2)
                .and_then(|size| size.checked_add(1))
                .ok_or(MemoryError::SizeOverflow)?,
        )?;
        bytes.push(1)?;
        for (index, unit) in units.iter().enumerate() {
            if index % 1024 == 0 {
                poll()?;
            }
            for byte in unit.to_be_bytes() {
                bytes.push(byte)?;
            }
        }
        poll()?;
        let (bytes, memory) = bytes.into_parts();
        Ok(Budgeted::new(Self(bytes), memory))
    }

    /// Copy a validated key into a separately reserved buffer without reinterpreting its bytes.
    pub fn clone_budgeted<E: From<MemoryError>>(
        &self,
        budget: &MemoryBudget,
        mut poll: impl FnMut() -> Result<(), E>,
    ) -> Result<Budgeted<Self>, E> {
        poll()?;
        let mut bytes = BudgetedVec::new(budget);
        bytes.reserve(self.0.len())?;
        for (index, byte) in self.0.iter().copied().enumerate() {
            if index % 1024 == 0 {
                poll()?;
            }
            bytes.push(byte)?;
        }
        poll()?;
        let (bytes, memory) = bytes.into_parts();
        Ok(Budgeted::new(Self(bytes), memory))
    }

    /// Compare persistent vocabulary order while checking long common prefixes in bounded chunks.
    pub fn cmp_with_control<E>(
        &self,
        other: &Self,
        poll: &mut dyn FnMut() -> Result<(), E>,
    ) -> Result<std::cmp::Ordering, E> {
        poll()?;
        for (left, right) in self.0.chunks(1024).zip(other.0.chunks(1024)) {
            poll()?;
            let order = left.cmp(right);
            if !order.is_eq() {
                return Ok(order);
            }
        }
        Ok(self.0.len().cmp(&other.0.len()))
    }
}

#[cfg(test)]
mod tests;
