//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Japanese attributes adapt the existing reserved stream without coupling to Korean policy.

use super::super::{KuromojiOutput, KuromojiToken};
use crate::morphology::filter::{
    covering_range, text_units, ComposingToken, FilterStream, FilterToken, Work,
};
use crate::AnalysisResult;
use std::ops::Range;
use uqa_core::memory::{Budgeted, MemoryBudget, MemoryReservation};

pub(crate) trait JapaneseToken: ComposingToken {
    fn generated(
        term: Budgeted<Vec<u16>>,
        first: &Self::Span,
        last: &Self::Span,
        increment: u32,
        context: &Self::Context,
        work: &mut Work<'_>,
    ) -> AnalysisResult<Budgeted<Self>>;
    fn attributes(&self) -> [Option<&str>; 6];
    fn part_of_speech(&self) -> AnalysisResult<Option<&str>>;
    fn reading(&self) -> AnalysisResult<Option<&str>>;
    fn base_form(&self) -> Option<&str> {
        self.attributes()[1]
    }
}
impl JapaneseToken for KuromojiToken {
    fn generated(
        term: Budgeted<Vec<u16>>,
        first: &Self::Span,
        last: &Self::Span,
        increment: u32,
        (): &(),
        _work: &mut Work<'_>,
    ) -> AnalysisResult<Budgeted<Self>> {
        let (term, memory) = term.into_parts();
        let mut token = Self::new(term, covering_range(first, last)?, None);
        token.position_increment = increment;
        Ok(Budgeted::new(token, memory))
    }

    fn reading(&self) -> AnalysisResult<Option<&str>> {
        self.errors.check(2)?;
        Ok(self.reading.as_deref())
    }
    fn part_of_speech(&self) -> AnalysisResult<Option<&str>> {
        self.errors.check(0)?;
        Ok(self.part_of_speech.as_deref())
    }
    fn attributes(&self) -> [Option<&str>; 6] {
        self.fields().map(|field| field.map(String::as_str))
    }
}
impl From<KuromojiOutput> for FilterStream<KuromojiToken> {
    fn from(output: KuromojiOutput) -> Self {
        Self {
            tokens: output.tokens,
            terminal: output.terminal,
            final_offset_utf16: output.final_offset_utf16,
            final_position_increment: output.final_position_increment,
            context: (),
        }
    }
}
impl From<FilterStream<KuromojiToken>> for KuromojiOutput {
    fn from(stream: FilterStream<KuromojiToken>) -> Self {
        Self {
            tokens: stream.tokens,
            terminal: stream.terminal,
            final_offset_utf16: stream.final_offset_utf16,
            final_position_increment: stream.final_position_increment,
        }
    }
}
impl FilterToken for KuromojiToken {
    type Context = ();
    fn term(&self) -> impl Iterator<Item = u16> + '_ {
        self.term_utf16.iter().copied()
    }
    fn term_len(&self, _work: &mut Work<'_>) -> AnalysisResult<usize> {
        Ok(self.term_utf16.len())
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
    fn keyword(&self) -> bool {
        self.keyword
    }
}

impl ComposingToken for KuromojiToken {
    type Span = Range<usize>;
    fn clone_reserved(
        &self,
        budget: &MemoryBudget,
        work: &mut Work<'_>,
    ) -> AnalysisResult<Budgeted<Self>> {
        self.clone_budgeted(budget, work.poll)
    }
    fn position_length(&self) -> u32 {
        self.position_length
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
        let span = covering_range(first, last)?;
        self.start_utf16 = span.start;
        self.end_utf16 = span.end;
        Ok(())
    }
}

pub(in crate::kuromoji) fn token_units<T: JapaneseToken>(
    token: &T,
    term: usize,
    previous: usize,
    work: &mut Work<'_>,
) -> AnalysisResult<usize> {
    let overflow =
        || super::super::error::invalid("Kuromoji filter", "UTF-16 output size overflow");
    let mut total = previous.checked_add(term).ok_or_else(overflow)?;
    for attribute in token.attributes().into_iter().flatten() {
        total = total
            .checked_add(text_units(attribute, work)?)
            .ok_or_else(overflow)?;
    }
    Ok(total)
}
