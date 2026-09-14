//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Lossless Nori-to-generic conversion, including hidden stream exhaustion state.

use super::{AnalysisToken, AnalyzedText, Morphology, TokenBatch};
use crate::nori::filters::stream::{covering_range, FilterToken};
use crate::nori::filters::Work;
use crate::nori::{KoreanMorphology, NoriOutput, NoriToken};
use crate::source::SourceProjection;
use crate::{AnalysisResult, FilteredText, TokenTerm};
use std::ops::Range;
use std::sync::Arc;
use uqa_core::memory::{Budgeted, MemoryBudget, MemoryReservation};

mod allocation;

impl super::native::NativeToken for NoriToken {
    fn take_term(&mut self) -> Vec<u16> {
        std::mem::take(&mut self.term_utf16)
    }

    fn into_fields(self) -> super::native::NativeFields {
        super::native::NativeFields {
            span: self.start_utf16..self.end_utf16,
            increment: self.position_increment,
            length: self.position_length,
            keyword: self.keyword,
            morphology: Morphology::Korean(KoreanMorphology {
                pos_type: self.pos_type,
                left_pos: self.left_pos,
                right_pos: self.right_pos,
                reading: self.reading,
                morphemes: self.morphemes,
                origin: self.origin,
            }),
        }
    }
}

impl AnalyzedText {
    pub(crate) fn from_nori(output: NoriOutput, input: &FilteredText<'_>) -> AnalysisResult<Self> {
        super::native::output(
            TokenBatch {
                tokens: output.tokens,
                final_position_increment: output.final_position_increment,
                terminal: output.terminal,
            },
            output.final_offset_utf16,
            input,
        )
    }
}

impl FilterToken for AnalysisToken {
    type Span = Option<Range<usize>>;
    type Context = Arc<Budgeted<SourceProjection>>;

    fn term(&self) -> impl Iterator<Item = u16> + '_ {
        self.term.utf16_units()
    }
    fn term_len(&self, work: &mut Work<'_>) -> AnalysisResult<usize> {
        if self.term.as_str().is_none() {
            return Ok(self.term.utf16_len());
        }
        let mut length = 0;
        for _ in self.term.utf16_units() {
            work.tick()?;
            length += 1;
        }
        Ok(length)
    }
    fn clone_reserved(
        &self,
        budget: &MemoryBudget,
        work: &mut Work<'_>,
    ) -> AnalysisResult<Budgeted<Self>> {
        self.clone_budgeted(budget, &mut *work.poll)
    }
    fn replace_term(
        &mut self,
        term: Budgeted<Vec<u16>>,
        memory: &mut MemoryReservation,
        context: &Self::Context,
        work: &mut Work<'_>,
    ) -> AnalysisResult<()> {
        let term = TokenTerm::from_utf16_budgeted(term, &mut *work.poll)?;
        let verbatim = if let Some(offsets) = &self.offsets {
            context.is_verbatim_with_control(&term, offsets, work.poll)?
        } else {
            false
        };
        if !self.term.eq_with_control(&term, work.poll)? {
            let old_bytes = self.term.allocation_bytes();
            let (term, allocation) = term.into_parts();
            drop(std::mem::replace(&mut self.term, term));
            drop(memory.split(old_bytes));
            memory.absorb(allocation);
        }
        self.verbatim = verbatim;
        Ok(())
    }
    fn refresh_context(
        &mut self,
        context: &Self::Context,
        work: &mut Work<'_>,
    ) -> AnalysisResult<()> {
        self.verbatim = if let Some(offsets) = &self.offsets {
            context.is_verbatim_with_control(&self.term, offsets, work.poll)?
        } else {
            false
        };
        Ok(())
    }
    fn position_length(&self) -> u32 {
        self.position_length
    }
    fn keyword(&self) -> bool {
        self.keyword
    }
    fn left_pos(&self) -> Option<crate::nori::POSTag> {
        self.korean_morphology().map(|value| value.left_pos)
    }
    fn reading(&self) -> Option<&str> {
        self.korean_morphology()
            .and_then(|value| value.reading.as_deref())
    }
    fn morphemes(&self) -> Option<&[crate::nori::NoriMorpheme]> {
        self.korean_morphology()
            .and_then(|value| value.morphemes.as_deref())
    }
    fn span(&self) -> Self::Span {
        self.filtered_utf16.clone()
    }
    fn cover(
        &mut self,
        first: &Self::Span,
        last: &Self::Span,
        context: &Self::Context,
        work: &mut Work<'_>,
    ) -> AnalysisResult<()> {
        self.filtered_utf16 = first
            .as_ref()
            .zip(last.as_ref())
            .map(|(first, last)| covering_range(first, last))
            .transpose()?;
        self.offsets = self
            .filtered_utf16
            .clone()
            .map(|range| context.project_with_control(range, work.poll))
            .transpose()?;
        self.refresh_context(context, work)
    }
}

#[cfg(test)]
mod tests;
