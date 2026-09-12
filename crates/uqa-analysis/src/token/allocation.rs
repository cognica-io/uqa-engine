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

pub(crate) use batch::TokenBatchAllocation;

#[cfg(test)]
mod tests;

pub(crate) struct TokenBuffer {
    pub(super) tokens: BudgetedVec<AnalysisToken>,
    terminal: Option<Box<AnalysisToken>>,
    pub(super) memory: MemoryReservation,
}

impl TokenBuffer {
    pub fn new(budget: &MemoryBudget) -> Self {
        Self {
            tokens: BudgetedVec::new(budget),
            terminal: None,
            memory: budget.empty_reservation(),
        }
    }

    pub fn push(&mut self, token: Budgeted<AnalysisToken>) -> AnalysisResult<()> {
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

    pub(crate) fn token(&self, index: usize) -> &AnalysisToken {
        &self.tokens[index]
    }

    pub(crate) fn set_terminal_box(&mut self, terminal: Budgeted<Box<AnalysisToken>>) {
        assert!(self.terminal.is_none());
        let (terminal, memory) = terminal.into_parts();
        self.terminal = Some(terminal);
        self.memory.absorb(memory);
    }

    pub(crate) fn set_terminal_token(
        &mut self,
        terminal: Budgeted<AnalysisToken>,
    ) -> AnalysisResult<()> {
        let payload = self.memory.budget().reserve(size_of::<AnalysisToken>())?;
        let (terminal, mut memory) = terminal.into_parts();
        let terminal = Box::new(terminal);
        memory.absorb(payload);
        self.set_terminal_box(Budgeted::new(terminal, memory));
        Ok(())
    }

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
        let (tokens, mut memory) = self.tokens.into_parts();
        memory.absorb(self.memory);
        let output = Budgeted::new(
            TokenBatch {
                tokens,
                final_position_increment,
                terminal: self.terminal,
            },
            memory,
        );
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
