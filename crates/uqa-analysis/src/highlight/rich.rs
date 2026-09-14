//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Match complete-source token spans under one retained compiled revision.

use super::{
    render::{merge_spans, render, Span},
    terms::Terms,
    HighlightOptions,
};
use crate::{AnalysisError, AnalysisResult, CompiledAnalyzer};
use uqa_core::memory::{Budgeted, BudgetedVec, MemoryBudget};

/// Highlight original source spans using one immutable analyzer for the source and each complete query input.
///
/// Matching preserves raw UTF-16 term identity. Markers use scalar-covering UTF-8 offsets, and overlapping spans merge only for presentation. Fragment sizes are targets; a selected match is always kept whole.
pub fn highlight_compiled(
    text: &str,
    query_inputs: &[String],
    analyzer: &CompiledAnalyzer,
    opts: &HighlightOptions,
) -> AnalysisResult<String> {
    Ok(highlight_compiled_budgeted(
        text,
        query_inputs,
        analyzer,
        opts,
        &MemoryBudget::new(usize::MAX),
        || Ok(()),
    )?
    .into_parts()
    .0)
}

/// Retain one runtime allowance through complete-source analysis and source-span rendering.
///
/// Query terms keep their moved token buffers while unused morphology/source state is released. Matching preserves raw UTF-16 identity, density ties preserve source order, and a selected match remains complete inside its fragment. Errors release partial output without releasing another allocation owner's reservations.
pub fn highlight_compiled_budgeted(
    text: &str,
    query_inputs: &[String],
    analyzer: &CompiledAnalyzer,
    opts: &HighlightOptions,
    budget: &MemoryBudget,
    mut poll: impl FnMut() -> AnalysisResult<()>,
) -> AnalysisResult<Budgeted<String>> {
    poll()?;
    if text.is_empty() || query_inputs.is_empty() {
        return crate::allocation::copy_text(text, budget, &mut poll);
    }
    let mut terms = Terms::new(budget);
    for query in query_inputs {
        poll()?;
        terms.append(
            analyzer.analyze_tokens_budgeted(query, budget, &mut poll)?,
            false,
            &mut poll,
        )?;
    }
    if terms.is_empty() {
        return crate::allocation::copy_text(text, budget, &mut poll);
    }
    terms.sort_unique(&mut poll)?;
    let source = analyzer.analyze_tokens_budgeted(text, budget, &mut poll)?;
    let mut spans = BudgetedVec::new(budget);
    for token in source.tokens() {
        poll()?;
        if !terms.contains(token.term(), &mut poll)? {
            continue;
        }
        let range = &token
            .offsets()
            .ok_or(AnalysisError::MissingTokenOffsets)?
            .utf8;
        if range.start > range.end {
            return Err(AnalysisError::InvalidTextSpan {
                start: range.start,
                end: range.end,
            });
        }
        for offset in [range.start, range.end] {
            if !text.is_char_boundary(offset) {
                return Err(AnalysisError::InvalidTextOffset {
                    coordinate: "UTF-8",
                    offset,
                    length: text.len(),
                });
            }
        }
        if !range.is_empty() {
            spans.push(Span::new(range.start, range.end))?;
        }
    }
    drop(source);
    drop(terms);
    merge_spans(&mut spans, &mut poll)?;
    render(text, spans, opts, budget, &mut poll)
}
