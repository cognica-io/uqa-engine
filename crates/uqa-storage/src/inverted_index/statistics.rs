//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Query statistics retain analyzer output, encoded terms and provider reads under one allowance.

use super::InvertedIndex;
use crate::{
    read_control::StorageReadControl, StorageBackendError, StorageBackendResult, TokenTermKey,
};
use uqa_analysis::AnalysisError;
use uqa_core::memory::BudgetedString;

pub(crate) fn field_name(
    field: &str,
    control: &StorageReadControl,
) -> StorageBackendResult<BudgetedString> {
    control.check()?;
    let mut output = BudgetedString::new(control.memory());
    output.reserve(field.len())?;
    for (offset, character) in field.chars().enumerate() {
        if offset % 1024 == 0 {
            control.check()?;
        }
        output.push(character)?;
    }
    control.check()?;
    Ok(output)
}

fn analysis_error(error: AnalysisError, control: &StorageReadControl) -> StorageBackendError {
    match error {
        AnalysisError::Memory(error) => StorageBackendError::Memory(error),
        AnalysisError::Cancelled => control.check().err().unwrap_or_else(|| {
            StorageBackendError::Cancelled(uqa_core::QueryCancelled::USER_REQUEST)
        }),
        error => StorageBackendError::Analysis(error),
    }
}

/// Count emitted query terms without constructing a second term collection.
pub fn query_term_count(
    index: &dyn InvertedIndex,
    field: &str,
    query: &str,
    control: &StorageReadControl,
) -> StorageBackendResult<usize> {
    control.check()?;
    let analyzer = index.search_analyzer_revision(field)?;
    let tokens = analyzer
        .analyze_tokens_budgeted(query, control.memory(), || {
            control.check().map_err(|_| AnalysisError::Cancelled)
        })
        .map_err(|error| analysis_error(error, control))?;
    control.check()?;
    Ok(tokens.tokens().len())
}

/// Sum frequencies in original field and emitted-term order, including repeated and raw UTF-16 terms. Each term key retires after its read; the analyzer output remains owned until the field is complete.
pub fn query_document_frequency(
    index: &dyn InvertedIndex,
    field: Option<&str>,
    query: &str,
    control: &StorageReadControl,
) -> StorageBackendResult<u64> {
    control.check()?;
    let mut frequency = 0_u64;
    let mut observe = |field: &str| -> StorageBackendResult<()> {
        control.check()?;
        let analyzer = index.search_analyzer_revision(field)?;
        let tokens = analyzer
            .analyze_tokens_budgeted(query, control.memory(), || {
                control.check().map_err(|_| AnalysisError::Cancelled)
            })
            .map_err(|error| analysis_error(error, control))?;
        for token in tokens.tokens() {
            control.check()?;
            let key = TokenTermKey::from_term_budgeted(token.term(), control.memory(), || {
                control.check()
            })?;
            frequency =
                frequency.saturating_add(index.doc_freq_key_budgeted(field, &key, control)?);
        }
        Ok(())
    };
    if let Some(field) = field {
        observe(field)?;
    } else {
        let fields = index.field_names_budgeted(control)?;
        for field in fields.iter() {
            observe(field)?;
        }
    }
    control.check()?;
    Ok(frequency)
}

#[cfg(test)]
mod tests;
