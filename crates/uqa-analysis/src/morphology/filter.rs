//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Shared term mutation, source context and allocation ownership for native morphology filters.

use crate::token::{
    allocation::{AllocatedToken, TokenBatchAllocation},
    TokenBatch,
};
use crate::{AnalysisError, AnalysisResult};
use std::ops::Range;
use uqa_core::memory::{Budgeted, BudgetedVec, MemoryBudget, MemoryReservation};

pub(crate) mod lowercase;

pub(crate) trait FilterToken: AllocatedToken + Sized {
    type Span;
    type Context;

    fn term(&self) -> impl Iterator<Item = u16> + '_;
    fn term_len(&self, work: &mut Work<'_>) -> AnalysisResult<usize>;
    fn clone_reserved(
        &self,
        budget: &MemoryBudget,
        work: &mut Work<'_>,
    ) -> AnalysisResult<Budgeted<Self>>;
    fn replace_term(
        &mut self,
        term: Budgeted<Vec<u16>>,
        memory: &mut MemoryReservation,
        context: &Self::Context,
        work: &mut Work<'_>,
    ) -> AnalysisResult<()>;
    fn refresh_context(
        &mut self,
        _context: &Self::Context,
        _work: &mut Work<'_>,
    ) -> AnalysisResult<()> {
        Ok(())
    }

    fn copy_term(
        &self,
        budget: &MemoryBudget,
        work: &mut Work<'_>,
    ) -> AnalysisResult<BudgetedVec<u16>> {
        let mut output = BudgetedVec::new(budget);
        output.reserve(self.term_len(work)?)?;
        for unit in self.term() {
            work.tick()?;
            output.push(unit)?;
        }
        Ok(output)
    }
    fn position_length(&self) -> u32;
    fn keyword(&self) -> bool;
    fn span(&self) -> Self::Span;
    fn cover(
        &mut self,
        first: &Self::Span,
        last: &Self::Span,
        context: &Self::Context,
        work: &mut Work<'_>,
    ) -> AnalysisResult<()>;
}

pub(crate) struct FilterStream<T: FilterToken> {
    pub tokens: Vec<T>,
    pub terminal: Option<Box<T>>,
    pub final_offset_utf16: usize,
    pub final_position_increment: u32,
    pub context: T::Context,
}

pub(crate) struct AllocatedStream<T: FilterToken> {
    pub batch: TokenBatchAllocation<T>,
    pub final_offset_utf16: usize,
    pub context: T::Context,
}

impl<T: FilterToken> AllocatedStream<T> {
    pub(crate) fn from_unreserved(
        input: FilterStream<T>,
        poll: &mut dyn FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<Self> {
        Ok(Self {
            batch: TokenBatchAllocation::from_unreserved_with_control(
                TokenBatch {
                    tokens: input.tokens,
                    terminal: input.terminal,
                    final_position_increment: input.final_position_increment,
                },
                poll,
            )?,
            final_offset_utf16: input.final_offset_utf16,
            context: input.context,
        })
    }

    pub(crate) fn from_budgeted(input: Budgeted<FilterStream<T>>) -> Self {
        let (input, memory) = input.into_parts();
        Self {
            batch: TokenBatchAllocation::from_budgeted(Budgeted::new(
                TokenBatch {
                    tokens: input.tokens,
                    terminal: input.terminal,
                    final_position_increment: input.final_position_increment,
                },
                memory,
            )),
            final_offset_utf16: input.final_offset_utf16,
            context: input.context,
        }
    }

    pub(crate) fn into_budgeted(self) -> Budgeted<FilterStream<T>> {
        let (batch, memory) = self.batch.into_budgeted().into_parts();
        Budgeted::new(
            FilterStream {
                tokens: batch.tokens,
                terminal: batch.terminal,
                final_position_increment: batch.final_position_increment,
                final_offset_utf16: self.final_offset_utf16,
                context: self.context,
            },
            memory,
        )
    }
}

pub(crate) fn covering_range(
    first: &Range<usize>,
    last: &Range<usize>,
) -> AnalysisResult<Range<usize>> {
    if first.start > last.end {
        return Err(AnalysisError::InvalidTextSpan {
            start: first.start,
            end: last.end,
        });
    }
    Ok(first.start..last.end)
}

pub(crate) struct Work<'a> {
    counter: usize,
    pub(crate) poll: &'a mut dyn FnMut() -> AnalysisResult<()>,
}

impl<'a> Work<'a> {
    pub fn new(poll: &'a mut dyn FnMut() -> AnalysisResult<()>) -> AnalysisResult<Self> {
        poll()?;
        Ok(Self { counter: 0, poll })
    }
    pub fn tick(&mut self) -> AnalysisResult<()> {
        self.counter = (self.counter + 1) % 1024;
        if self.counter == 0 {
            (self.poll)()?;
        }
        Ok(())
    }
    pub fn finish(&mut self) -> AnalysisResult<()> {
        (self.poll)()
    }
}

pub(crate) fn text_units(text: &str, work: &mut Work<'_>) -> AnalysisResult<usize> {
    let mut length = 0;
    for character in text.chars() {
        work.tick()?;
        length += character.len_utf16();
    }
    Ok(length)
}
