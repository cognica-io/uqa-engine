//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Structured analysis tokens and end-of-stream position state.

use std::ops::Range;
#[cfg(feature = "nori")]
use std::sync::Arc;

use serde::Serialize;
#[cfg(feature = "nori")]
use uqa_core::memory::Budgeted;

#[cfg(test)]
use crate::FilteredText;
use crate::{AnalysisError, AnalysisResult, SourceOffsets, TokenTerm};

pub(crate) mod allocation;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AnalysisToken {
    pub(crate) term: TokenTerm,
    pub(crate) offsets: Option<SourceOffsets>,
    pub(crate) position_increment: u32,
    pub(crate) position_length: u32,
    pub(crate) keyword: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    filtered_utf16: Option<Range<usize>>,
    #[cfg(feature = "nori")]
    #[serde(skip_serializing_if = "Option::is_none")]
    korean_morphology: Option<crate::nori::KoreanMorphology>,
    #[serde(skip)]
    verbatim: bool,
}

impl AnalysisToken {
    pub fn term(&self) -> &TokenTerm {
        &self.term
    }

    pub fn offsets(&self) -> Option<&SourceOffsets> {
        self.offsets.as_ref()
    }

    pub fn position_increment(&self) -> u32 {
        self.position_increment
    }

    pub fn position_length(&self) -> u32 {
        self.position_length
    }

    pub fn is_keyword(&self) -> bool {
        self.keyword
    }

    /// Exact tokenizer coordinates before character-filter source correction, when supplied.
    pub fn filtered_utf16(&self) -> Option<&Range<usize>> {
        self.filtered_utf16.as_ref()
    }

    #[cfg(feature = "nori")]
    pub fn korean_morphology(&self) -> Option<&crate::nori::KoreanMorphology> {
        self.korean_morphology.as_ref()
    }

    #[cfg(test)]
    pub(crate) fn from_source(
        input: &FilteredText<'_>,
        range: Range<usize>,
    ) -> AnalysisResult<Self> {
        let budget = uqa_core::memory::MemoryBudget::new(usize::MAX);
        input.prepare_coordinates(&budget, &mut || Ok(()))?;
        Ok(
            Self::from_source_budgeted(input, range, &budget, &mut || Ok(()))?
                .into_parts()
                .0,
        )
    }

    fn term_only(term: String) -> Self {
        Self {
            term: term.into(),
            offsets: None,
            position_increment: 1,
            position_length: 1,
            keyword: false,
            filtered_utf16: None,
            #[cfg(feature = "nori")]
            korean_morphology: None,
            verbatim: false,
        }
    }

    #[cfg(any(test, feature = "nori"))]
    pub(crate) fn replace_term(&mut self, term: TokenTerm) {
        if term != self.term {
            self.verbatim = false;
            self.term = term;
        }
    }

    #[cfg(test)]
    pub(crate) fn substring(&self, range: Range<usize>) -> Self {
        let mut token = Self {
            term: self.term.substring(range.clone()),
            offsets: self.offsets.clone(),
            position_increment: self.position_increment,
            position_length: self.position_length,
            keyword: self.keyword,
            filtered_utf16: self.filtered_utf16.clone(),
            #[cfg(feature = "nori")]
            korean_morphology: self.korean_morphology.clone(),
            verbatim: self.verbatim,
        };
        if self.verbatim {
            if let Some(offsets) = &self.offsets {
                let original = self.term.as_str().expect("verbatim Unicode input");
                let start_utf16 = original[..range.start].encode_utf16().count();
                let length_utf16 = token.term.utf16_len();
                token.offsets = Some(SourceOffsets {
                    utf8: offsets.utf8.start + range.start..offsets.utf8.start + range.end,
                    utf16: offsets.utf16.start + start_utf16
                        ..offsets.utf16.start + start_utf16 + length_utf16,
                });
                if let Some(filtered) = &self.filtered_utf16 {
                    if filtered.len() == self.term.utf16_len() {
                        token.filtered_utf16 = Some(
                            filtered.start + start_utf16
                                ..filtered.start + start_utf16 + length_utf16,
                        );
                    }
                }
            }
        }
        token
    }
}

/// A complete analyzed input with explicit token graph and source end state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AnalyzedText {
    #[serde(flatten)]
    pub(crate) batch: TokenBatch,
    pub(crate) final_offsets: SourceOffsets,
    #[cfg(feature = "nori")]
    #[serde(skip)]
    pub(crate) projection: Arc<Budgeted<crate::source::SourceProjection>>,
}

impl AnalyzedText {
    pub fn tokens(&self) -> &[AnalysisToken] {
        &self.batch.tokens
    }

    pub fn into_tokens(self) -> Vec<AnalysisToken> {
        self.batch.tokens
    }

    pub fn into_terms(self) -> AnalysisResult<Vec<String>> {
        self.batch.into_terms()
    }

    pub fn final_offsets(&self) -> &SourceOffsets {
        &self.final_offsets
    }

    pub fn final_position_increment(&self) -> u32 {
        self.batch.final_position_increment
    }

    #[cfg(test)]
    pub(crate) fn from_source(
        tokens: Vec<AnalysisToken>,
        input: &FilteredText<'_>,
    ) -> AnalysisResult<Self> {
        let batch = TokenBatch {
            tokens,
            final_position_increment: 0,
            terminal: None,
        };
        batch.validate_positions()?;
        Ok(Self {
            batch,
            final_offsets: input.final_offsets(),
            #[cfg(feature = "nori")]
            projection: input.projection(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct TokenBatch {
    pub tokens: Vec<AnalysisToken>,
    pub final_position_increment: u32,
    #[serde(skip)]
    pub terminal: Option<Box<AnalysisToken>>,
}

impl TokenBatch {
    pub fn from_terms(terms: Vec<String>) -> Self {
        Self {
            tokens: terms.into_iter().map(AnalysisToken::term_only).collect(),
            final_position_increment: 0,
            terminal: None,
        }
    }

    pub fn into_terms(self) -> AnalysisResult<Vec<String>> {
        self.tokens
            .into_iter()
            .map(|token| token.term.into_string())
            .collect()
    }

    #[cfg(any(test, feature = "nori"))]
    pub fn validate_positions(&self) -> AnalysisResult<()> {
        self.validate_positions_with_control(&mut || Ok(()))
    }

    pub(crate) fn validate_positions_with_control(
        &self,
        poll: &mut dyn FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<()> {
        let mut position = -1_i64;
        for (index, token) in self.tokens.iter().enumerate() {
            if index % 1024 == 0 {
                poll()?;
            }
            if token.position_length == 0 || (position < 0 && token.position_increment == 0) {
                return Err(AnalysisError::InvalidTokenPosition);
            }
            position = position
                .checked_add(i64::from(token.position_increment))
                .ok_or(AnalysisError::TokenPositionOverflow)?;
            let position =
                u32::try_from(position).map_err(|_| AnalysisError::TokenPositionOverflow)?;
            position
                .checked_add(token.position_length)
                .ok_or(AnalysisError::TokenPositionOverflow)?;
        }
        let final_position = position
            .checked_add(i64::from(self.final_position_increment))
            .ok_or(AnalysisError::TokenPositionOverflow)?;
        if final_position > i64::from(u32::MAX) {
            return Err(AnalysisError::TokenPositionOverflow);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;

#[cfg(feature = "nori")]
mod korean;
