//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Lossless Nori-to-generic conversion, including hidden stream exhaustion state.

use super::{AnalysisToken, AnalyzedText, TokenBatch};
use crate::nori::filters::stream::{covering_range, FilterToken};
use crate::nori::{KoreanMorphology, NoriOutput, NoriToken};
use crate::source::SourceProjection;
use crate::{AnalysisError, AnalysisResult, FilteredText, TokenTerm};
use std::borrow::Cow;
use std::ops::Range;
use std::sync::Arc;

impl AnalysisToken {
    fn from_nori(token: NoriToken, input: &FilteredText<'_>) -> AnalysisResult<Self> {
        let filtered_utf16 = token.start_utf16..token.end_utf16;
        let offsets = input.source_covering_offsets_utf16(filtered_utf16.clone())?;
        let term = TokenTerm::from_utf16(token.term_utf16);
        let verbatim = term.as_str().is_some_and(|text| {
            input.original().get(offsets.utf8.clone()) == Some(text)
                && term.utf16_len() == offsets.utf16.len()
        });
        Ok(Self {
            term,
            offsets: Some(offsets),
            position_increment: token.position_increment,
            position_length: token.position_length,
            keyword: token.keyword,
            filtered_utf16: Some(filtered_utf16),
            korean_morphology: Some(KoreanMorphology {
                pos_type: token.pos_type,
                left_pos: token.left_pos,
                right_pos: token.right_pos,
                reading: token.reading,
                morphemes: token.morphemes,
                origin: token.origin,
            }),
            verbatim,
        })
    }
}

impl AnalyzedText {
    pub(crate) fn from_nori(output: NoriOutput, input: &FilteredText<'_>) -> AnalysisResult<Self> {
        let length = input.as_str().encode_utf16().count();
        if output.final_offset_utf16 != length {
            return Err(AnalysisError::MismatchedAnalysisInput {
                expected_utf16: output.final_offset_utf16,
                actual_utf16: length,
            });
        }
        let batch = TokenBatch {
            tokens: output
                .tokens
                .into_iter()
                .map(|token| AnalysisToken::from_nori(token, input))
                .collect::<AnalysisResult<_>>()?,
            final_position_increment: output.final_position_increment,
            terminal: output
                .terminal
                .map(|token| AnalysisToken::from_nori(*token, input))
                .transpose()?
                .map(Box::new),
        };
        batch.validate_positions()?;
        Ok(Self {
            batch,
            final_offsets: input.final_offsets(),
            projection: input.projection(),
        })
    }
}

impl FilterToken for AnalysisToken {
    type Span = Option<Range<usize>>;
    type Context = Arc<SourceProjection>;

    fn term(&self) -> Cow<'_, [u16]> {
        self.term.utf16()
    }
    fn term_len(&self) -> usize {
        self.term.utf16_len()
    }
    fn replace_term(&mut self, term: Vec<u16>, context: &Self::Context) {
        self.replace_term(TokenTerm::from_utf16(term));
        self.verbatim = self
            .offsets
            .as_ref()
            .is_some_and(|offsets| context.is_verbatim(&self.term, offsets));
    }
    fn mutate_term(
        &mut self,
        context: &Self::Context,
        operation: impl FnOnce(&mut Vec<u16>, Option<&str>) -> AnalysisResult<()>,
    ) -> AnalysisResult<()> {
        let mut units = self.term.utf16().into_owned();
        operation(
            &mut units,
            self.korean_morphology
                .as_ref()
                .and_then(|value| value.reading.as_deref()),
        )?;
        FilterToken::replace_term(self, units, context);
        Ok(())
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
    fn left_pos(&self) -> Option<crate::nori::POSTag> {
        self.korean_morphology.as_ref().map(|value| value.left_pos)
    }
    fn reading(&self) -> Option<&str> {
        self.korean_morphology
            .as_ref()
            .and_then(|value| value.reading.as_deref())
    }
    fn morphemes(&self) -> Option<&[crate::nori::NoriMorpheme]> {
        self.korean_morphology
            .as_ref()
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
    ) -> AnalysisResult<()> {
        self.filtered_utf16 = first
            .as_ref()
            .zip(last.as_ref())
            .map(|(first, last)| covering_range(first, last))
            .transpose()?;
        self.offsets = self
            .filtered_utf16
            .clone()
            .map(|range| context.project(range))
            .transpose()?;
        self.verbatim = self
            .offsets
            .as_ref()
            .is_some_and(|offsets| context.is_verbatim(&self.term, offsets));
        Ok(())
    }
}

#[cfg(test)]
mod tests;
