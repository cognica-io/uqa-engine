//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Shared staging of immutable analysis results before any provider mutation.

use std::collections::BTreeMap;

use uqa_analysis::{
    AnalysisError, AnalyzedText, AnalyzerFingerprint, CompiledAnalyzer, SourceOffsets,
    TokenLengthPolicy, TokenTerm,
};
use uqa_core::{memory::OwnedMap, TokenOccurrence, TokenOffsets};

use crate::{StorageBackendError, StorageBackendResult, TokenTermKey};

mod budgeted;
pub use budgeted::analyze_index_field_budgeted;

/// Metadata published with a complete document field's occurrences. The field's retained index revision owns the matching descriptor and resources.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndexedFieldMetadata {
    pub analyzer_fingerprint: AnalyzerFingerprint,
    pub occurrence_format_version: u8,
    pub length_policy: TokenLengthPolicy,
    pub length: u64,
    pub final_offsets: TokenOffsets,
    pub final_position_increment: u32,
}

impl IndexedFieldMetadata {
    pub fn new<T>(analyzer: &CompiledAnalyzer, field: &AnalyzedField<T>) -> Self {
        Self {
            analyzer_fingerprint: analyzer.descriptor().fingerprint(),
            occurrence_format_version: crate::clustered_postings::OCCURRENCE_FORMAT_VERSION,
            length_policy: analyzer.descriptor().length_policy(),
            length: field.length,
            final_offsets: field.final_offsets,
            final_position_increment: field.final_position_increment,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnalyzedField<T = BTreeMap<TokenTermKey, Vec<TokenOccurrence>>> {
    pub length: u64,
    pub terms: T,
    pub final_offsets: TokenOffsets,
    pub final_position_increment: u32,
}

/// Controlled occurrence projection retains complete ordered nodes as well as encoded term and occurrence buffers. Ordinary field analysis keeps its existing map representation.
pub type RetainedAnalyzedField = AnalyzedField<OwnedMap<TokenTermKey, Vec<TokenOccurrence>>>;

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
    analyze_index_field_with_poll(analyzer, text, || Ok(()))
}

/// Analyze and project one source field while observing the caller's cancellation token.
pub fn analyze_index_field_cancellable(
    analyzer: &CompiledAnalyzer,
    text: &str,
    cancellation: &uqa_core::CancellationToken,
) -> StorageBackendResult<AnalyzedField> {
    analyze_index_field_with_poll(analyzer, text, || {
        cancellation.check().map_err(|_| AnalysisError::Cancelled)
    })
    .map_err(|error| match error {
        StorageBackendError::Analysis(AnalysisError::Cancelled) => {
            StorageBackendError::Cancelled(uqa_core::QueryCancelled)
        }
        other => other,
    })
}

fn analyze_index_field_with_poll(
    analyzer: &CompiledAnalyzer,
    text: &str,
    mut poll: impl FnMut() -> Result<(), AnalysisError>,
) -> StorageBackendResult<AnalyzedField> {
    let output = analyzer
        .analyze_tokens_budgeted(
            text,
            &uqa_core::memory::MemoryBudget::new(usize::MAX),
            &mut poll,
        )?
        .into_parts()
        .0;
    let mut terms = BTreeMap::<TokenTermKey, Vec<TokenOccurrence>>::new();
    let metadata = project_index_tokens(analyzer, &output, &mut poll, |term, occurrence, _| {
        terms
            .entry(TokenTermKey::from_term(term))
            .or_default()
            .push(occurrence);
        Ok(())
    })?;
    Ok(AnalyzedField {
        length: metadata.length,
        terms,
        final_offsets: metadata.final_offsets,
        final_position_increment: metadata.final_position_increment,
    })
}

fn project_index_tokens<P: FnMut() -> Result<(), AnalysisError>>(
    analyzer: &CompiledAnalyzer,
    output: &AnalyzedText,
    poll: &mut P,
    mut push: impl FnMut(&TokenTerm, TokenOccurrence, &mut P) -> StorageBackendResult<()>,
) -> StorageBackendResult<IndexedFieldMetadata> {
    let mut metadata = IndexedFieldMetadata {
        analyzer_fingerprint: analyzer.descriptor().fingerprint(),
        occurrence_format_version: crate::clustered_postings::OCCURRENCE_FORMAT_VERSION,
        length_policy: analyzer.descriptor().length_policy(),
        length: 0,
        final_offsets: source_offsets(output.final_offsets())?,
        final_position_increment: output.final_position_increment(),
    };
    let mut position = -1_i64;
    for token in output.tokens() {
        poll()?;
        position = position
            .checked_add(i64::from(token.position_increment()))
            .ok_or(AnalysisError::TokenPositionOverflow)?;
        let position = u32::try_from(position).map_err(|_| AnalysisError::TokenPositionOverflow)?;
        let offsets = token.offsets().map(source_offsets).transpose()?;
        if offsets.is_some_and(|offsets| {
            offsets.end_utf8 > metadata.final_offsets.end_utf8
                || offsets.end_utf16 > metadata.final_offsets.end_utf16
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
        push(token.term(), occurrence, poll)?;
        if metadata.length_policy == TokenLengthPolicy::EmittedTokens
            || token.position_increment() > 0
        {
            metadata.length = metadata
                .length
                .checked_add(1)
                .ok_or_else(|| super::counter_error("document token count"))?;
        }
    }
    poll()?;
    Ok(metadata)
}

mod query;
pub use query::{
    analyze_query_graph, analyze_query_graph_budgeted, analyze_query_terms,
    analyze_query_terms_budgeted,
};

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

#[cfg(test)]
#[path = "analysis/index_tests.rs"]
mod index_tests;
