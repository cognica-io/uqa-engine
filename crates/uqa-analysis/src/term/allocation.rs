//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Canonical term conversion retains both encodings while they coexist.

use uqa_core::memory::{Budgeted, BudgetedString, BudgetedVec, MemoryBudget, MemoryError};

use super::{Representation, TokenTerm};
use crate::AnalysisResult;

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
