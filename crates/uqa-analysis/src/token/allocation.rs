//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Token buffers retain their terms and morphology until the result changes owner.

use std::ops::Range;

use uqa_core::memory::{Budgeted, BudgetedVec, MemoryBudget, MemoryReservation};

use super::{AnalysisToken, AnalyzedText, TokenBatch};
use crate::{AnalysisResult, FilteredText, SourceOffsets};

mod batch;
mod cloning;

#[cfg(feature = "nori")]
pub(crate) use batch::AllocatedToken;
pub(crate) use batch::{TokenBatchAllocation, TokenBatchInput};

impl AnalyzedText {
    /// Transfer emitted tokens and their leases while dropping the source projection.
    pub(crate) fn into_token_input(input: Budgeted<Self>) -> TokenBatchInput {
        let (input, memory) = input.into_parts();
        TokenBatchAllocation::from_budgeted(Budgeted::new(input.batch, memory)).into_input()
    }
}

impl AnalysisToken {
    /// Move the term's allocation after releasing unused token attributes.
    pub(crate) fn into_term_budgeted(input: Budgeted<Self>) -> Budgeted<crate::TokenTerm> {
        let (mut token, mut memory) = input.into_parts();
        let term = std::mem::replace(&mut token.term, crate::TokenTerm::from(String::new()));
        let allocation = memory.split(term.allocation_bytes());
        drop(token);
        drop(memory);
        Budgeted::new(term, allocation)
    }
}

#[cfg(test)]
mod tests;

pub(crate) struct TokenBuffer<T = AnalysisToken> {
    pub(super) tokens: BudgetedVec<T>,
    terminal: Option<Box<T>>,
    pub(super) memory: MemoryReservation,
}

impl<T> TokenBuffer<T> {
    pub fn new(budget: &MemoryBudget) -> Self {
        Self {
            tokens: BudgetedVec::new(budget),
            terminal: None,
            memory: budget.empty_reservation(),
        }
    }

    pub fn push(&mut self, token: Budgeted<T>) -> AnalysisResult<()> {
        self.tokens.reserve(1)?;
        let (token, memory) = token.into_parts();
        self.memory.absorb(memory);
        self.tokens.push(token)?;
        Ok(())
    }

    pub(crate) fn reserve_tokens(&mut self, count: usize) -> AnalysisResult<()> {
        self.tokens.reserve(count)?;
        Ok(())
    }

    pub(crate) fn len(&self) -> usize {
        self.tokens.len()
    }

    pub(crate) fn token(&self, index: usize) -> &T {
        &self.tokens[index]
    }

    pub(crate) fn set_terminal_box(&mut self, terminal: Budgeted<Box<T>>) {
        assert!(self.terminal.is_none());
        let (terminal, memory) = terminal.into_parts();
        self.terminal = Some(terminal);
        self.memory.absorb(memory);
    }

    pub(crate) fn set_terminal_token(&mut self, terminal: Budgeted<T>) -> AnalysisResult<()> {
        let payload = self.memory.budget().reserve(size_of::<T>())?;
        let (terminal, mut memory) = terminal.into_parts();
        let terminal = Box::new(terminal);
        memory.absorb(payload);
        self.set_terminal_box(Budgeted::new(terminal, memory));
        Ok(())
    }

    pub(crate) fn into_batch(self, final_position_increment: u32) -> Budgeted<TokenBatch<T>> {
        let (tokens, mut memory) = self.tokens.into_parts();
        memory.absorb(self.memory);
        Budgeted::new(
            TokenBatch {
                tokens,
                final_position_increment,
                terminal: self.terminal,
            },
            memory,
        )
    }
}

impl TokenBuffer {
    #[cfg(feature = "nori")]
    pub(super) fn set_terminal(&mut self, token: AnalysisToken) -> AnalysisResult<()> {
        self.memory.grow(std::mem::size_of::<AnalysisToken>())?;
        self.terminal = Some(Box::new(token));
        Ok(())
    }

    pub fn finish(
        self,
        input: &FilteredText<'_>,
        final_position_increment: u32,
        poll: &mut dyn FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<Budgeted<AnalyzedText>> {
        poll()?;
        #[cfg(feature = "nori")]
        let projection = input.projection_budgeted(self.memory.budget(), poll)?;
        self.finish_retained(
            input.final_offsets(),
            final_position_increment,
            #[cfg(feature = "nori")]
            projection,
            poll,
        )
    }

    fn finish_retained(
        self,
        final_offsets: SourceOffsets,
        final_position_increment: u32,
        #[cfg(feature = "nori")] projection: std::sync::Arc<
            Budgeted<crate::source::SourceProjection>,
        >,
        poll: &mut dyn FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<Budgeted<AnalyzedText>> {
        let (batch, memory) = self
            .finish_batch(final_position_increment, poll)?
            .into_parts();
        Ok(Budgeted::new(
            AnalyzedText {
                batch,
                final_offsets,
                #[cfg(feature = "nori")]
                projection,
            },
            memory,
        ))
    }

    pub(crate) fn finish_batch(
        self,
        final_position_increment: u32,
        poll: &mut dyn FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<Budgeted<TokenBatch>> {
        poll()?;
        let output = self.into_batch(final_position_increment);
        output.validate_positions_with_control(poll)?;
        poll()?;
        Ok(output)
    }
}

impl AnalysisToken {
    pub(crate) fn from_source_budgeted(
        input: &FilteredText<'_>,
        range: Range<usize>,
        budget: &MemoryBudget,
        poll: &mut dyn FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<Budgeted<Self>> {
        poll()?;
        let offsets = input.source_offsets(range.clone())?;
        let filtered_utf16 = Some(input.filtered_utf16(range.clone())?);
        let source = &input.as_str()[range];
        let verbatim = input.original().get(offsets.utf8.clone()) == Some(source);
        let (term, memory) = crate::allocation::copy_text(source, budget, poll)?.into_parts();
        Ok(Budgeted::new(
            Self {
                term: term.into(),
                offsets: Some(offsets),
                position_increment: 1,
                position_length: 1,
                keyword: false,
                filtered_utf16,
                #[cfg(feature = "nori")]
                korean_morphology: None,
                verbatim,
            },
            memory,
        ))
    }
}
