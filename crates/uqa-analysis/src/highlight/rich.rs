//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Match complete-source token spans under one retained compiled revision.

use std::collections::HashSet;

use super::{render_highlights, HighlightOptions};
use crate::{AnalysisError, AnalysisResult, CompiledAnalyzer, TokenTerm};

/// Highlight original source spans using one immutable analyzer for the source and each complete query input.
///
/// Matching preserves raw UTF-16 term identity. Markers use scalar-covering UTF-8 offsets, and overlapping spans merge only for presentation. Fragment sizes are targets; a selected match is always kept whole.
pub fn highlight_compiled(
    text: &str,
    query_inputs: &[String],
    analyzer: &CompiledAnalyzer,
    opts: &HighlightOptions,
) -> AnalysisResult<String> {
    if text.is_empty() || query_inputs.is_empty() {
        return Ok(text.to_owned());
    }
    let mut terms = HashSet::<TokenTerm>::new();
    for query in query_inputs {
        terms.extend(
            analyzer
                .analyze_tokens(query)?
                .into_tokens()
                .into_iter()
                .map(|token| token.term),
        );
    }
    if terms.is_empty() {
        return Ok(text.to_owned());
    }
    let source = analyzer.analyze_tokens(text)?;
    let mut spans = Vec::new();
    for token in source.tokens() {
        if !terms.contains(token.term()) {
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
            spans.push((range.start, range.end));
        }
    }
    spans.sort_unstable();
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for (start, end) in spans {
        if let Some(previous) = merged.last_mut().filter(|previous| start < previous.1) {
            previous.1 = previous.1.max(end);
        } else {
            merged.push((start, end));
        }
    }
    // Walk disjoint source ranges once to obtain the renderer's Unicode character coordinates.
    let (mut byte, mut character) = (0, 0);
    for span in &mut merged {
        character += text[byte..span.0].chars().count();
        let start = character;
        character += text[span.0..span.1].chars().count();
        byte = span.1;
        *span = (start, character);
    }
    Ok(render_highlights(text, &merged, opts))
}
