//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! One Korean filter algorithm over native and generic token attributes.

use std::ops::Range;

use super::Work;
use crate::nori::{NoriMorpheme, NoriOutput, NoriToken, POSTag};
use crate::token::{
    allocation::{AllocatedToken, TokenBatchAllocation},
    TokenBatch,
};
use crate::{AnalysisError, AnalysisResult};
use uqa_core::memory::{Budgeted, BudgetedVec, MemoryBudget, MemoryReservation};

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

    fn reading_form(
        &mut self,
        term_units: usize,
        memory: &mut MemoryReservation,
        context: &Self::Context,
        work: &mut Work<'_>,
    ) -> AnalysisResult<()> {
        if let Some(reading) = self.reading() {
            let term = crate::nori::tokenizer::allocation::encode(
                reading,
                term_units,
                memory.budget(),
                work.poll,
            )?;
            self.replace_term(term, memory, context, work)
        } else {
            self.refresh_context(context, work)
        }
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
    fn lowercase(
        &mut self,
        model: Option<&crate::nori::NoriDictionary>,
        memory: &mut MemoryReservation,
        context: &Self::Context,
        work: &mut Work<'_>,
    ) -> AnalysisResult<()> {
        let mut term = self.copy_term(memory.budget(), work)?;
        super::lowercase::apply(&mut term, model, work)?;
        let (term, allocation) = term.into_parts();
        self.replace_term(Budgeted::new(term, allocation), memory, context, work)
    }
    fn position_length(&self) -> u32;
    fn keyword(&self) -> bool;
    fn left_pos(&self) -> Option<POSTag>;
    fn reading(&self) -> Option<&str>;
    fn morphemes(&self) -> Option<&[NoriMorpheme]>;
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

impl From<NoriOutput> for FilterStream<NoriToken> {
    fn from(output: NoriOutput) -> Self {
        Self {
            tokens: output.tokens,
            terminal: output.terminal,
            final_offset_utf16: output.final_offset_utf16,
            final_position_increment: output.final_position_increment,
            context: (),
        }
    }
}

impl From<FilterStream<NoriToken>> for NoriOutput {
    fn from(stream: FilterStream<NoriToken>) -> Self {
        Self {
            tokens: stream.tokens,
            terminal: stream.terminal,
            final_offset_utf16: stream.final_offset_utf16,
            final_position_increment: stream.final_position_increment,
        }
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

impl FilterToken for NoriToken {
    type Span = Range<usize>;
    type Context = ();

    fn term(&self) -> impl Iterator<Item = u16> + '_ {
        self.term_utf16.iter().copied()
    }
    fn term_len(&self, _work: &mut Work<'_>) -> AnalysisResult<usize> {
        Ok(self.term_utf16.len())
    }
    fn clone_reserved(
        &self,
        budget: &MemoryBudget,
        work: &mut Work<'_>,
    ) -> AnalysisResult<Budgeted<Self>> {
        self.clone_budgeted(budget, work.poll)
    }
    fn replace_term(
        &mut self,
        term: Budgeted<Vec<u16>>,
        memory: &mut MemoryReservation,
        (): &(),
        _work: &mut Work<'_>,
    ) -> AnalysisResult<()> {
        let bytes = self.term_utf16.capacity() * size_of::<u16>();
        let (term, allocation) = term.into_parts();
        drop(std::mem::replace(&mut self.term_utf16, term));
        drop(memory.split(bytes));
        memory.absorb(allocation);
        Ok(())
    }
    fn lowercase(
        &mut self,
        model: Option<&crate::nori::NoriDictionary>,
        _memory: &mut MemoryReservation,
        (): &(),
        work: &mut Work<'_>,
    ) -> AnalysisResult<()> {
        super::lowercase::apply(&mut self.term_utf16, model, work)
    }

    fn reading_form(
        &mut self,
        term_units: usize,
        memory: &mut MemoryReservation,
        (): &(),
        work: &mut Work<'_>,
    ) -> AnalysisResult<()> {
        let Some(reading) = &self.reading else {
            return Ok(());
        };
        if term_units <= self.term_utf16.capacity() {
            self.term_utf16.clear();
            for unit in reading.encode_utf16() {
                work.tick()?;
                self.term_utf16.push(unit);
            }
            return Ok(());
        }
        let term = crate::nori::tokenizer::allocation::encode(
            reading,
            term_units,
            memory.budget(),
            work.poll,
        )?;
        self.replace_term(term, memory, &(), work)
    }
    fn position_length(&self) -> u32 {
        self.position_length
    }
    fn keyword(&self) -> bool {
        self.keyword
    }
    fn left_pos(&self) -> Option<POSTag> {
        Some(self.left_pos)
    }
    fn reading(&self) -> Option<&str> {
        self.reading.as_deref()
    }
    fn morphemes(&self) -> Option<&[NoriMorpheme]> {
        self.morphemes.as_deref()
    }
    fn span(&self) -> Self::Span {
        self.start_utf16..self.end_utf16
    }
    fn cover(
        &mut self,
        first: &Self::Span,
        last: &Self::Span,
        (): &(),
        _work: &mut Work<'_>,
    ) -> AnalysisResult<()> {
        let range = covering_range(first, last)?;
        self.start_utf16 = range.start;
        self.end_utf16 = range.end;
        Ok(())
    }
}
