//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Batch mutation retains allocation leases until the corresponding buffers are freed.

use uqa_core::memory::{Budgeted, MemoryBudget, MemoryError, MemoryReservation};

use super::{AnalysisToken, AnalyzedText, TokenBatch};
use crate::{AnalysisError, AnalysisResult, TokenTerm};

mod input;
pub(crate) use input::TokenBatchInput;

pub(crate) struct TokenBatchAllocation {
    batch: TokenBatch,
    memory: MemoryReservation,
}

impl AnalysisToken {
    pub(crate) fn allocation_bytes(
        &self,
        poll: &mut dyn FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<usize> {
        poll()?;
        let bytes = self.term.allocation_bytes();
        #[cfg(feature = "nori")]
        let bytes = bytes
            .checked_add(
                self.korean_morphology
                    .as_ref()
                    .map_or(Ok(0), |morphology| morphology.allocation_bytes(poll))?,
            )
            .ok_or(MemoryError::SizeOverflow)?;
        Ok(bytes)
    }
}

impl TokenBatch {
    pub(super) fn allocation_bytes(
        &self,
        poll: &mut dyn FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<usize> {
        poll()?;
        let mut bytes = self.tokens.capacity() * size_of::<AnalysisToken>();
        for token in &self.tokens {
            bytes = bytes
                .checked_add(token.allocation_bytes(poll)?)
                .ok_or(MemoryError::SizeOverflow)?;
        }
        if let Some(terminal) = &self.terminal {
            let attributes = terminal.allocation_bytes(poll)?;
            bytes = bytes
                .checked_add(size_of::<AnalysisToken>())
                .and_then(|bytes| bytes.checked_add(attributes))
                .ok_or(MemoryError::SizeOverflow)?;
        }
        Ok(bytes)
    }
}

impl AnalyzedText {
    pub(crate) fn into_unlimited(self) -> AnalysisResult<Budgeted<Self>> {
        let bytes = self.batch.allocation_bytes(&mut || Ok(()))?;
        let memory = MemoryBudget::new(usize::MAX).reserve(bytes)?;
        Ok(Budgeted::new(self, memory))
    }
}

impl TokenBatchAllocation {
    pub(crate) fn from_budgeted(input: Budgeted<TokenBatch>) -> Self {
        let (batch, memory) = input.into_parts();
        Self { batch, memory }
    }

    /// Ordinary APIs transfer already materialized input into an unlimited allocation owner.
    pub(crate) fn from_unreserved(batch: TokenBatch) -> AnalysisResult<Self> {
        let mut output = Self {
            batch,
            memory: MemoryBudget::new(usize::MAX).empty_reservation(),
        };
        output
            .memory
            .grow(output.batch.allocation_bytes(&mut || Ok(()))?)?;
        Ok(output)
    }

    pub(crate) fn tokens(&self) -> &[AnalysisToken] {
        &self.batch.tokens
    }

    pub(crate) fn budget(&self) -> &MemoryBudget {
        self.memory.budget()
    }

    pub(crate) fn map_terms(
        mut self,
        mut transform: impl FnMut(
            &AnalysisToken,
            &MemoryBudget,
            &mut dyn FnMut() -> AnalysisResult<()>,
        ) -> AnalysisResult<Option<Budgeted<TokenTerm>>>,
        poll: &mut dyn FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<Self> {
        let budget = self.budget().clone();
        for token in &mut self.batch.tokens {
            poll()?;
            let Some(replacement) = transform(token, &budget, poll)? else {
                continue;
            };
            if token.term.eq_with_control(&replacement, poll)? {
                continue;
            }
            let old_bytes = token.term.allocation_bytes();
            let (replacement, memory) = replacement.into_parts();
            let original = std::mem::replace(&mut token.term, replacement);
            token.verbatim = false;
            drop(original);
            drop(self.memory.split(old_bytes));
            self.memory.absorb(memory);
        }
        Ok(self)
    }

    pub(crate) fn retain(
        mut self,
        mut keep: impl FnMut(
            &AnalysisToken,
            &mut dyn FnMut() -> AnalysisResult<()>,
        ) -> AnalysisResult<bool>,
        poll: &mut dyn FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<Self> {
        let mut retained = 0;
        let mut removed_bytes = 0usize;
        let mut skipped = 0u32;
        let mut trailing_removed = false;
        for index in 0..self.batch.tokens.len() {
            poll()?;
            let token = &mut self.batch.tokens[index];
            if keep(token, poll)? {
                token.position_increment = token
                    .position_increment
                    .checked_add(skipped)
                    .ok_or(AnalysisError::TokenPositionOverflow)?;
                skipped = 0;
                trailing_removed = false;
                self.batch.tokens.swap(retained, index);
                retained += 1;
            } else {
                skipped = skipped
                    .checked_add(token.position_increment)
                    .ok_or(AnalysisError::TokenPositionOverflow)?;
                removed_bytes = removed_bytes
                    .checked_add(token.allocation_bytes(poll)?)
                    .ok_or(MemoryError::SizeOverflow)?;
                trailing_removed = true;
            }
        }
        let terminal = if trailing_removed && self.batch.terminal.is_none() {
            let token = self.batch.tokens.pop().expect("trailing removed token");
            let bytes = token.allocation_bytes(poll)?;
            removed_bytes -= bytes;
            Some(Budgeted::new(token, self.memory.split(bytes)))
        } else {
            None
        };
        self.batch.tokens.truncate(retained);
        if retained == 0 {
            let bytes = self.batch.tokens.capacity() * size_of::<AnalysisToken>();
            drop(std::mem::take(&mut self.batch.tokens));
            drop(self.memory.split(bytes));
        }
        drop(self.memory.split(removed_bytes));
        if let Some(terminal) = terminal {
            self.memory.grow(size_of::<AnalysisToken>())?;
            let (terminal, memory) = terminal.into_parts();
            self.batch.terminal = Some(Box::new(terminal));
            self.memory.absorb(memory);
        }
        self.batch.final_position_increment = self
            .batch
            .final_position_increment
            .checked_add(skipped)
            .ok_or(AnalysisError::TokenPositionOverflow)?;
        Ok(self)
    }

    pub(crate) fn into_input(self) -> TokenBatchInput {
        TokenBatchInput::new(self.batch, self.memory)
    }

    pub(crate) fn finish(
        self,
        poll: &mut dyn FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<Budgeted<TokenBatch>> {
        self.batch.validate_positions_with_control(poll)?;
        poll()?;
        Ok(Budgeted::new(self.batch, self.memory))
    }
}
