//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Tokenizer emission with original source spans.

use std::ops::Range;
use uqa_core::memory::{Budgeted, BudgetedVec, MemoryBudget};

use super::PreparedTokenizer;
use crate::token::allocation::TokenBuffer;
use crate::{AnalysisResult, AnalysisToken, AnalyzedText, FilteredText};

pub(super) fn tokenize(
    tokenizer: &PreparedTokenizer,
    input: &FilteredText<'_>,
) -> AnalysisResult<AnalyzedText> {
    Ok(
        tokenize_budgeted(tokenizer, input, &input.unbounded_budget(), &mut || Ok(()))?
            .into_parts()
            .0,
    )
}

pub(super) fn tokenize_budgeted(
    tokenizer: &PreparedTokenizer,
    input: &FilteredText<'_>,
    budget: &MemoryBudget,
    poll: &mut dyn FnMut() -> AnalysisResult<()>,
) -> AnalysisResult<Budgeted<AnalyzedText>> {
    let source = input.clone();
    let input = &source;
    input.prepare_coordinates(budget, poll)?;
    let text = input.as_str();
    let mut tokens = TokenBuffer::new(budget);
    match tokenizer {
        #[cfg(feature = "nori")]
        PreparedTokenizer::Nori(tokenizer) => {
            let output = tokenizer.tokenize_budgeted(
                text,
                crate::nori::NoriLimits::default(),
                budget,
                &mut || poll(),
            )?;
            return AnalyzedText::from_nori_budgeted(output, input, poll);
        }
        PreparedTokenizer::Whitespace => {
            for_each_word(text, poll, |range, poll| {
                tokens.push(AnalysisToken::from_source_budgeted(
                    input, range, budget, poll,
                )?)
            })?;
        }
        PreparedTokenizer::Matches(expression) => {
            let mut occurrences = expression.find_iter(text);
            loop {
                poll()?;
                let Some(matched) = occurrences.next() else {
                    break;
                };
                tokens.push(AnalysisToken::from_source_budgeted(
                    input,
                    matched.range(),
                    budget,
                    poll,
                )?)?;
            }
        }
        PreparedTokenizer::NGram { min_gram, max_gram } => {
            for_each_word(text, poll, |word, poll| {
                emit_grams(
                    input,
                    word,
                    (*min_gram, *max_gram),
                    &mut tokens,
                    budget,
                    poll,
                )
            })?;
        }
        PreparedTokenizer::Pattern(expression) => {
            let mut start = 0;
            let mut separators = expression.find_iter(text);
            loop {
                poll()?;
                let Some(separator) = separators.next() else {
                    break;
                };
                if start < separator.start() {
                    tokens.push(AnalysisToken::from_source_budgeted(
                        input,
                        start..separator.start(),
                        budget,
                        poll,
                    )?)?;
                }
                start = separator.end();
            }
            if start < text.len() {
                tokens.push(AnalysisToken::from_source_budgeted(
                    input,
                    start..text.len(),
                    budget,
                    poll,
                )?)?;
            }
        }
        PreparedTokenizer::Keyword => {
            if !text.is_empty() {
                tokens.push(AnalysisToken::from_source_budgeted(
                    input,
                    0..text.len(),
                    budget,
                    poll,
                )?)?;
            }
        }
    }
    tokens.finish(input, 0, poll)
}

fn emit_grams(
    input: &FilteredText<'_>,
    word: Range<usize>,
    (min_gram, max_gram): (usize, usize),
    tokens: &mut TokenBuffer,
    budget: &MemoryBudget,
    poll: &mut dyn FnMut() -> AnalysisResult<()>,
) -> AnalysisResult<()> {
    let mut boundaries = BudgetedVec::new(budget);
    for (index, (offset, _)) in input.as_str()[word.clone()].char_indices().enumerate() {
        if index % 1024 == 0 {
            poll()?;
        }
        boundaries.push(word.start + offset)?;
    }
    boundaries.push(word.end)?;
    let length = boundaries.len() - 1;
    for n in min_gram..=max_gram.min(length) {
        for start in 0..=length - n {
            tokens.push(AnalysisToken::from_source_budgeted(
                input,
                boundaries[start]..boundaries[start + n],
                budget,
                poll,
            )?)?;
        }
    }
    Ok(())
}

fn for_each_word(
    text: &str,
    poll: &mut dyn FnMut() -> AnalysisResult<()>,
    mut emit: impl FnMut(Range<usize>, &mut dyn FnMut() -> AnalysisResult<()>) -> AnalysisResult<()>,
) -> AnalysisResult<()> {
    let mut start = None;
    for (index, (offset, character)) in text.char_indices().enumerate() {
        if index % 1024 == 0 {
            poll()?;
        }
        if character.is_whitespace() {
            if let Some(start) = start.take() {
                emit(start..offset, poll)?;
            }
        } else {
            start.get_or_insert(offset);
        }
    }
    if let Some(start) = start {
        emit(start..text.len(), poll)?;
    }
    Ok(())
}
