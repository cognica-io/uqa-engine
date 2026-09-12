//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Shared staging of immutable analysis results before any provider mutation.

use std::collections::BTreeMap;

use uqa_analysis::{AnalysisError, CompiledAnalyzer, SourceOffsets, TokenLengthPolicy};
use uqa_core::{TokenOccurrence, TokenOffsets};

use crate::{StorageBackendError, StorageBackendResult, TokenTermKey};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnalyzedField {
    pub length: u64,
    pub terms: BTreeMap<TokenTermKey, Vec<TokenOccurrence>>,
    pub final_offsets: TokenOffsets,
    pub final_position_increment: u32,
}

/// Analyze one complete source field with a resolved revision, retaining every emitted graph edge.
///
/// ```
/// use uqa_analysis::{Analyzer, AnalyzerLimits, AnalyzerResources, TokenLengthPolicy};
/// use uqa_storage::{inverted_index::analyze_index_field, TokenTermKey};
/// let config: Analyzer = serde_json::from_str(r#"{"tokenizer":{"type":"whitespace"},"token_filters":[{"type":"synonym","synonyms":{"a":["a","a"]}}]}"#)?;
/// let compiled = AnalyzerResources::new(AnalyzerLimits::default())
///     .compile_with_length_policy(&config, TokenLengthPolicy::DiscountOverlaps)?;
/// let field = analyze_index_field(&compiled, "a")?;
/// assert_eq!(field.length, 1);
/// assert_eq!(field.terms[&TokenTermKey::from_text("a")].len(), 3);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn analyze_index_field(
    analyzer: &CompiledAnalyzer,
    text: &str,
) -> StorageBackendResult<AnalyzedField> {
    let output = analyzer.analyze_tokens(text)?;
    let mut staged = AnalyzedField {
        length: 0,
        terms: BTreeMap::new(),
        final_offsets: source_offsets(output.final_offsets())?,
        final_position_increment: output.final_position_increment(),
    };
    let mut position = -1_i64;
    for token in output.tokens() {
        position = position
            .checked_add(i64::from(token.position_increment()))
            .ok_or(AnalysisError::TokenPositionOverflow)?;
        let position = u32::try_from(position).map_err(|_| AnalysisError::TokenPositionOverflow)?;
        let offsets = token.offsets().map(source_offsets).transpose()?;
        if offsets.is_some_and(|offsets| {
            offsets.end_utf8 > staged.final_offsets.end_utf8
                || offsets.end_utf16 > staged.final_offsets.end_utf16
        }) {
            return Err(StorageBackendError::Other(
                "token occurrence exceeds original source".into(),
            ));
        }
        let occurrence = TokenOccurrence {
            position,
            position_length: token.position_length(),
            offsets,
        };
        occurrence
            .validate()
            .map_err(|error| StorageBackendError::Other(error.to_string()))?;
        staged
            .terms
            .entry(TokenTermKey::from_term(token.term()))
            .or_default()
            .push(occurrence);
        if analyzer.descriptor().length_policy() == TokenLengthPolicy::EmittedTokens
            || token.position_increment() > 0
        {
            staged.length = staged
                .length
                .checked_add(1)
                .ok_or_else(|| super::counter_error("document token count"))?;
        }
    }
    Ok(staged)
}

fn source_offsets(offsets: &SourceOffsets) -> StorageBackendResult<TokenOffsets> {
    fn offset(value: usize) -> StorageBackendResult<u64> {
        u64::try_from(value).map_err(|_| super::counter_error("source offset"))
    }
    Ok(TokenOffsets {
        start_utf8: offset(offsets.utf8.start)?,
        end_utf8: offset(offsets.utf8.end)?,
        start_utf16: offset(offsets.utf16.start)?,
        end_utf16: offset(offsets.utf16.end)?,
    })
}
