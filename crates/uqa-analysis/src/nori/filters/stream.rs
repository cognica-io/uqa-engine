//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Korean attribute policy over shared native and common filter ownership.

use super::Work;
use crate::morphology::filter::{covering_range, FilterToken as StreamToken};
pub(crate) use crate::morphology::filter::{AllocatedStream, FilterStream};
use crate::nori::{NoriMorpheme, NoriOutput, NoriToken, POSTag};
use crate::AnalysisResult;
use std::ops::Range;
use uqa_core::memory::{Budgeted, MemoryBudget, MemoryReservation};

pub(crate) trait FilterToken: StreamToken {
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
    fn left_pos(&self) -> Option<POSTag>;
    fn reading(&self) -> Option<&str>;
    fn morphemes(&self) -> Option<&[NoriMorpheme]>;
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

impl StreamToken for NoriToken {
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
    fn position_length(&self) -> u32 {
        self.position_length
    }
    fn keyword(&self) -> bool {
        self.keyword
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

impl FilterToken for NoriToken {
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
    fn left_pos(&self) -> Option<POSTag> {
        Some(self.left_pos)
    }
    fn reading(&self) -> Option<&str> {
        self.reading.as_deref()
    }
    fn morphemes(&self) -> Option<&[NoriMorpheme]> {
        self.morphemes.as_deref()
    }
}
