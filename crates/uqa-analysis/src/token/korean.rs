//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Lossless Nori-to-generic conversion, including hidden stream exhaustion state.

use super::{AnalysisToken, AnalyzedText, Morphology, TokenBatch};
use crate::nori::filters::stream::FilterToken;
use crate::nori::{KoreanMorphology, NoriOutput, NoriToken};
use crate::{AnalysisResult, FilteredText};

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
}

#[cfg(test)]
mod tests;
