//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Lossless Nori-to-generic conversion, including hidden stream exhaustion state.

use super::{AnalysisToken, AnalyzedText, TokenBatch};
use crate::nori::{KoreanMorphology, NoriOutput, NoriToken};
use crate::{AnalysisError, AnalysisResult, FilteredText, TokenTerm};

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
        })
    }
}
