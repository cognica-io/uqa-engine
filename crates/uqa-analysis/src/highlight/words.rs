//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native Unicode word scans preserve the legacy independently analyzed word contract.

use super::{
    render::{render, Span},
    terms::{require_scalar, Terms},
    HighlightOptions,
};
use crate::{
    character_class::{class, contains},
    token_filter::lowercase,
    AnalysisError, AnalysisResult, Analyzer,
};
use regex_syntax::hir::ClassUnicode;
use std::sync::OnceLock;
use uqa_core::memory::{Budgeted, BudgetedVec, MemoryBudget};

fn word_class() -> AnalysisResult<&'static ClassUnicode> {
    static CLASS: OnceLock<Result<ClassUnicode, String>> = OnceLock::new();
    CLASS
        .get_or_init(|| class(r"\w"))
        .as_ref()
        .map_err(|message| AnalysisError::BuiltInRegex {
            component: "highlighter word scanner",
            message: message.clone(),
        })
}

/// Highlight independently analyzed source words with one runtime allocation allowance.
///
/// Query inputs are borrowed scalar strings. An explicit uncompiled analyzer retains per-call resource reload behavior. Without one, full lowercase matches the existing word-scanning contract. The callback covers native word classification, analysis, term lookup, span selection, and rendering; library searches inside configured analyzer stages still run between callback checks.
pub fn highlight_words_budgeted<'a>(
    text: &str,
    query_inputs: impl IntoIterator<Item = &'a str>,
    analyzer: Option<&Analyzer>,
    opts: &HighlightOptions,
    budget: &MemoryBudget,
    mut poll: impl FnMut() -> AnalysisResult<()>,
) -> AnalysisResult<Budgeted<String>> {
    poll()?;
    let mut query_inputs = query_inputs.into_iter().peekable();
    if text.is_empty() || query_inputs.peek().is_none() {
        return crate::allocation::copy_text(text, budget, &mut poll);
    }
    let mut terms = Terms::new(budget);
    for query in query_inputs {
        poll()?;
        if let Some(analyzer) = analyzer {
            terms.append(
                analyzer.analyze_tokens_budgeted(query, budget, &mut poll)?,
                true,
                &mut poll,
            )?;
        } else {
            terms.push(lowercase::lower_text_budgeted(
                query,
                lowercase::prepare()?,
                budget,
                &mut poll,
            )?)?;
        }
    }
    if terms.is_empty() {
        return crate::allocation::copy_text(text, budget, &mut poll);
    }
    terms.sort_unique(&mut poll)?;
    let class = word_class()?;
    let mut spans = BudgetedVec::new(budget);
    let mut start = None;
    for (index, (byte, character)) in text.char_indices().enumerate() {
        if index % 1024 == 0 {
            poll()?;
        }
        if contains(class, character) {
            start.get_or_insert(byte);
        } else if let Some(start) = start.take() {
            if hit(&text[start..byte], &terms, analyzer, budget, &mut poll)? {
                spans.push(Span::new(start, byte))?;
            }
        }
    }
    if let Some(start) = start {
        if hit(&text[start..], &terms, analyzer, budget, &mut poll)? {
            spans.push(Span::new(start, text.len()))?;
        }
    }
    drop(terms);
    render(text, spans, opts, budget, &mut poll)
}

fn hit(
    word: &str,
    terms: &Terms,
    analyzer: Option<&Analyzer>,
    budget: &MemoryBudget,
    poll: &mut dyn FnMut() -> AnalysisResult<()>,
) -> AnalysisResult<bool> {
    if let Some(analyzer) = analyzer {
        let output = analyzer.analyze_tokens_budgeted(word, budget, &mut *poll)?;
        // The scalar-only legacy projection fails before checking any token for a hit.
        for token in output.tokens() {
            require_scalar(token.term(), poll)?;
        }
        for token in output.tokens() {
            if terms.contains(token.term(), poll)? {
                return Ok(true);
            }
        }
        Ok(false)
    } else {
        let term = lowercase::lower_text_budgeted(word, lowercase::prepare()?, budget, poll)?;
        terms.contains(&term, poll)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_word_class_matches_the_pinned_regex_for_every_scalar() {
        let regex = regex::Regex::new(r"\w").unwrap();
        let class = word_class().unwrap();
        for character in (0..=0x0010_ffff).filter_map(char::from_u32) {
            let mut encoded = [0; 4];
            assert_eq!(
                contains(class, character),
                regex.is_match(character.encode_utf8(&mut encoded)),
                "{character:?}"
            );
        }
    }
}
