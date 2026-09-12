//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Canonical term conversion retains both encodings while they coexist.

use uqa_core::memory::{Budgeted, BudgetedString, BudgetedVec, MemoryBudget, MemoryError};

use super::{Representation, TokenTerm};
use crate::AnalysisResult;

/// The same character boundary in the term's stored representation and UTF-16.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TermBoundary {
    pub offset: usize,
    pub utf16: usize,
}

pub(crate) enum TermBuffer {
    Unicode(BudgetedString),
    UTF16(BudgetedVec<u16>),
}

impl TermBuffer {
    pub fn new(input: &TokenTerm, budget: &MemoryBudget) -> Self {
        if input.as_str().is_some() {
            Self::Unicode(BudgetedString::new(budget))
        } else {
            Self::UTF16(BudgetedVec::new(budget))
        }
    }

    pub fn push(&mut self, character: Result<char, u16>) -> AnalysisResult<()> {
        match (self, character) {
            (Self::Unicode(text), Ok(character)) => text.push(character)?,
            (Self::UTF16(units), Ok(character)) => {
                for unit in character.encode_utf16(&mut [0; 2]) {
                    units.push(*unit)?;
                }
            }
            (Self::UTF16(units), Err(unit)) => units.push(unit)?,
            (Self::Unicode(_), Err(_)) => unreachable!("scalar input preserves scalar output"),
        }
        Ok(())
    }

    pub fn finish(
        self,
        poll: &mut dyn FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<Budgeted<TokenTerm>> {
        poll()?;
        match self {
            Self::Unicode(text) => {
                let (text, memory) = text.into_parts();
                Ok(Budgeted::new(TokenTerm::from(text), memory))
            }
            Self::UTF16(units) => {
                let (units, memory) = units.into_parts();
                TokenTerm::from_utf16_budgeted(Budgeted::new(units, memory), poll)
            }
        }
    }
}

impl TokenTerm {
    pub(crate) fn allocation_bytes(&self) -> usize {
        match &self.0 {
            Representation::Unicode(text) => text.capacity(),
            Representation::UTF16(units) => units.capacity() * size_of::<u16>(),
        }
    }

    pub(crate) fn character_count_with_control(
        &self,
        poll: &mut dyn FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<usize> {
        poll()?;
        let mut length = 0usize;
        for (index, _) in self.characters().enumerate() {
            if index % 1024 == 0 {
                poll()?;
            }
            length += 1;
        }
        Ok(length)
    }

    /// Copy this term into independently reserved storage without changing scalar or raw-unit identity.
    pub fn clone_budgeted(
        &self,
        budget: &MemoryBudget,
        mut poll: impl FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<Budgeted<Self>> {
        match &self.0 {
            Representation::Unicode(text) => {
                let (text, memory) =
                    crate::allocation::copy_text(text, budget, &mut poll)?.into_parts();
                Ok(Budgeted::new(Self::from(text), memory))
            }
            Representation::UTF16(units) => {
                let (units, memory) =
                    crate::allocation::copy_units(units, budget, &mut poll)?.into_parts();
                Ok(Budgeted::new(Self(Representation::UTF16(units)), memory))
            }
        }
    }

    pub(crate) fn boundaries_budgeted(
        &self,
        budget: &MemoryBudget,
        poll: &mut dyn FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<Budgeted<Vec<TermBoundary>>> {
        poll()?;
        let length = self
            .character_count_with_control(poll)?
            .checked_add(1)
            .ok_or(MemoryError::SizeOverflow)?;
        let scalar = self.as_str().is_some();
        let mut output = BudgetedVec::new(budget);
        output.reserve(length)?;
        let mut boundary = TermBoundary {
            offset: 0,
            utf16: 0,
        };
        output.push(boundary)?;
        for (index, character) in self.characters().enumerate() {
            if index % 1024 == 0 {
                poll()?;
            }
            let utf16 = character.map_or(1, char::len_utf16);
            boundary.utf16 += utf16;
            boundary.offset += if scalar {
                character.expect("scalar representation").len_utf8()
            } else {
                utf16
            };
            output.push(boundary)?;
        }
        let (output, memory) = output.into_parts();
        Ok(Budgeted::new(output, memory))
    }

    pub(crate) fn substring_budgeted(
        &self,
        range: std::ops::Range<usize>,
        budget: &MemoryBudget,
        poll: &mut dyn FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<Budgeted<Self>> {
        match &self.0 {
            Representation::Unicode(text) => {
                let (text, memory) =
                    crate::allocation::copy_text(&text[range], budget, poll)?.into_parts();
                Ok(Budgeted::new(Self::from(text), memory))
            }
            Representation::UTF16(units) => Self::from_utf16_budgeted(
                crate::allocation::copy_units(&units[range], budget, poll)?,
                poll,
            ),
        }
    }

    pub(crate) fn eq_with_control(
        &self,
        other: &Self,
        poll: &mut dyn FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<bool> {
        fn compare<T: PartialEq>(
            left: &[T],
            right: &[T],
            poll: &mut dyn FnMut() -> AnalysisResult<()>,
        ) -> AnalysisResult<bool> {
            poll()?;
            if left.len() != right.len() {
                return Ok(false);
            }
            for (left, right) in left.chunks(1024).zip(right.chunks(1024)) {
                poll()?;
                if left != right {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        match (&self.0, &other.0) {
            (Representation::Unicode(left), Representation::Unicode(right)) => {
                compare(left.as_bytes(), right.as_bytes(), poll)
            }
            (Representation::UTF16(left), Representation::UTF16(right)) => {
                compare(left, right, poll)
            }
            _ => {
                poll()?;
                Ok(false)
            }
        }
    }

    /// A representation order for internal lookup, with canonical term equality.
    pub(crate) fn cmp_with_control(
        &self,
        other: &Self,
        poll: &mut dyn FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<std::cmp::Ordering> {
        fn compare<T: Ord>(
            left: &[T],
            right: &[T],
            poll: &mut dyn FnMut() -> AnalysisResult<()>,
        ) -> AnalysisResult<std::cmp::Ordering> {
            poll()?;
            for (left, right) in left.chunks(1024).zip(right.chunks(1024)) {
                poll()?;
                let order = left.cmp(right);
                if !order.is_eq() {
                    return Ok(order);
                }
            }
            Ok(left.len().cmp(&right.len()))
        }
        match (&self.0, &other.0) {
            (Representation::Unicode(left), Representation::Unicode(right)) => {
                compare(left.as_bytes(), right.as_bytes(), poll)
            }
            (Representation::UTF16(left), Representation::UTF16(right)) => {
                compare(left, right, poll)
            }
            (Representation::Unicode(_), Representation::UTF16(_)) => {
                poll()?;
                Ok(std::cmp::Ordering::Less)
            }
            (Representation::UTF16(_), Representation::Unicode(_)) => {
                poll()?;
                Ok(std::cmp::Ordering::Greater)
            }
        }
    }

    /// Consume reserved UTF-16 units, preserving isolated surrogates without replacement.
    ///
    /// Valid input reserves the exact UTF-8 buffer before decoding and releases the UTF-16 lease only after freeing its buffer. The input must carry the reservation for its capacity. Cancellation is checked during both validation and conversion.
    pub fn from_utf16_budgeted(
        input: Budgeted<Vec<u16>>,
        mut poll: impl FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<Budgeted<Self>> {
        poll()?;
        let mut length = 0usize;
        for (index, character) in char::decode_utf16(input.iter().copied()).enumerate() {
            if index % 1024 == 0 {
                poll()?;
            }
            let Ok(character) = character else {
                let (units, memory) = input.into_parts();
                return Ok(Budgeted::new(Self(Representation::UTF16(units)), memory));
            };
            length = length
                .checked_add(character.len_utf8())
                .ok_or(MemoryError::SizeOverflow)?;
        }
        let (units, memory) = input.into_parts();
        let budget = memory.budget().clone();
        let input = Budgeted::new(units, memory);
        let mut output = BudgetedString::new(&budget);
        output.reserve(length)?;
        for (index, character) in char::decode_utf16(input.iter().copied()).enumerate() {
            if index % 1024 == 0 {
                poll()?;
            }
            output.push(character.expect("validated UTF-16"))?;
        }
        poll()?;
        drop(input);
        let (text, memory) = output.into_parts();
        Ok(Budgeted::new(Self::from(text), memory))
    }
}

#[cfg(test)]
mod tests;
