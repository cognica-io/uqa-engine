//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Query analysis retains exact term keys and occurrences under the caller's allocation owner.

use super::source_offsets;
use crate::{StorageBackendError, StorageBackendResult, TokenTermKey};
use uqa_analysis::{AnalysisError, AnalysisResult, CompiledAnalyzer};
use uqa_core::{
    memory::{Budgeted, BudgetedVec, MemoryBudget, MemoryReservation},
    TokenOccurrence,
};

struct QueryBuffer<T> {
    // Destroy records before releasing the key-buffer reservations they retain.
    entries: BudgetedVec<T>,
    keys: MemoryReservation,
}
impl<T> QueryBuffer<T> {
    fn new(count: usize, budget: &MemoryBudget) -> AnalysisResult<Self> {
        let mut entries = BudgetedVec::new(budget);
        entries.reserve(count)?;
        Ok(Self {
            entries,
            keys: budget.empty_reservation(),
        })
    }
    fn push(&mut self, entry: Budgeted<T>) -> AnalysisResult<()> {
        self.entries.reserve(1)?;
        let (entry, memory) = entry.into_parts();
        self.keys.absorb(memory);
        self.entries.push(entry)?;
        Ok(())
    }
    fn finish(self) -> Budgeted<Vec<T>> {
        let (entries, mut memory) = self.entries.into_parts();
        memory.absorb(self.keys);
        Budgeted::new(entries, memory)
    }
}

/// Analyze a complete query without flattening raw terms or changing repeated-term accounting.
pub fn analyze_query_terms(
    analyzer: &CompiledAnalyzer,
    text: &str,
) -> StorageBackendResult<Vec<TokenTermKey>> {
    Ok(
        analyze_query_terms_budgeted(analyzer, text, &MemoryBudget::new(usize::MAX), || Ok(()))?
            .into_parts()
            .0,
    )
}

/// Retain one allowance through complete analysis and the returned ordered key buffers.
///
/// The result owns the key vector and every encoded key. Analysis scratch, morphology and source projections are released before return. Borrowed inputs and immutable analyzer resources remain separately owned; callback and allocation failures publish no partial query.
pub fn analyze_query_terms_budgeted(
    analyzer: &CompiledAnalyzer,
    text: &str,
    budget: &MemoryBudget,
    mut poll: impl FnMut() -> AnalysisResult<()>,
) -> StorageBackendResult<Budgeted<Vec<TokenTermKey>>> {
    let stream = analyzer.analyze_tokens_budgeted(text, budget, &mut poll)?;
    let mut output = QueryBuffer::new(stream.tokens().len(), budget)?;
    for token in stream.tokens() {
        poll()?;
        output.push(TokenTermKey::from_term_budgeted(
            token.term(),
            budget,
            &mut poll,
        )?)?;
    }
    poll()?;
    Ok(output.finish())
}

/// Analyze a complete phrase once, preserving emitted order, duplicates, holes and graph edges.
pub fn analyze_query_graph(
    analyzer: &CompiledAnalyzer,
    text: &str,
) -> StorageBackendResult<Vec<(TokenTermKey, TokenOccurrence)>> {
    Ok(
        analyze_query_graph_budgeted(analyzer, text, &MemoryBudget::new(usize::MAX), || Ok(()))?
            .into_parts()
            .0,
    )
}

/// Retain analysis and complete lossless graph records under the same caller allowance.
///
/// The returned guard owns the record vector and all encoded term keys. Consumers can keep this guard alive while reserving graph traversal, cursors and output from the same `MemoryBudget`. The callback also covers position/offset validation and key encoding.
pub fn analyze_query_graph_budgeted(
    analyzer: &CompiledAnalyzer,
    text: &str,
    budget: &MemoryBudget,
    mut poll: impl FnMut() -> AnalysisResult<()>,
) -> StorageBackendResult<Budgeted<Vec<(TokenTermKey, TokenOccurrence)>>> {
    let stream = analyzer.analyze_tokens_budgeted(text, budget, &mut poll)?;
    let mut output = QueryBuffer::new(stream.tokens().len(), budget)?;
    let mut position = -1_i64;
    for token in stream.tokens() {
        poll()?;
        position = position
            .checked_add(i64::from(token.position_increment()))
            .ok_or(AnalysisError::TokenPositionOverflow)?;
        let occurrence = TokenOccurrence {
            position: u32::try_from(position).map_err(|_| AnalysisError::TokenPositionOverflow)?,
            position_length: token.position_length(),
            offsets: token.offsets().map(source_offsets).transpose()?,
        };
        occurrence
            .validate()
            .map_err(|error| StorageBackendError::Other(error.to_string()))?;
        let (key, memory) =
            TokenTermKey::from_term_budgeted(token.term(), budget, &mut poll)?.into_parts();
        output.push(Budgeted::new((key, occurrence), memory))?;
    }
    poll()?;
    Ok(output.finish())
}

#[cfg(test)]
mod tests;
