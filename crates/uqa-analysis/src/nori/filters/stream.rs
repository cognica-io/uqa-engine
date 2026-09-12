//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! One Korean filter algorithm over native and generic token attributes.

use std::borrow::Cow;
use std::ops::Range;

use crate::nori::{NoriMorpheme, NoriOutput, NoriToken, POSTag};
use crate::{AnalysisError, AnalysisResult};

pub(crate) trait FilterToken: Clone {
    type Span;
    type Context;

    fn term(&self) -> Cow<'_, [u16]>;
    fn term_len(&self) -> usize;
    fn replace_term(&mut self, term: Vec<u16>, context: &Self::Context);
    fn mutate_term(
        &mut self,
        context: &Self::Context,
        operation: impl FnOnce(&mut Vec<u16>, Option<&str>) -> AnalysisResult<()>,
    ) -> AnalysisResult<()>;
    fn increment(&self) -> u32;
    fn set_increment(&mut self, increment: u32);
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
    ) -> AnalysisResult<()>;
}

pub(crate) struct FilterStream<T: FilterToken> {
    pub tokens: Vec<T>,
    pub terminal: Option<Box<T>>,
    pub final_offset_utf16: usize,
    pub final_position_increment: u32,
    pub context: T::Context,
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

    fn term(&self) -> Cow<'_, [u16]> {
        Cow::Borrowed(&self.term_utf16)
    }
    fn term_len(&self) -> usize {
        self.term_utf16.len()
    }
    fn replace_term(&mut self, term: Vec<u16>, (): &()) {
        self.term_utf16 = term;
    }
    fn mutate_term(
        &mut self,
        (): &(),
        operation: impl FnOnce(&mut Vec<u16>, Option<&str>) -> AnalysisResult<()>,
    ) -> AnalysisResult<()> {
        operation(&mut self.term_utf16, self.reading.as_deref())
    }
    fn increment(&self) -> u32 {
        self.position_increment
    }
    fn set_increment(&mut self, increment: u32) {
        self.position_increment = increment;
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
    fn cover(&mut self, first: &Self::Span, last: &Self::Span, (): &()) -> AnalysisResult<()> {
        let range = covering_range(first, last)?;
        self.start_utf16 = range.start;
        self.end_utf16 = range.end;
        Ok(())
    }
}
