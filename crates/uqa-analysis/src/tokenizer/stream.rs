//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Tokenizer emission with original source spans.

use std::ops::Range;

use regex::Regex;

use super::{letter_re, standard_word_re, validate_gram_bounds, Tokenizer};
use crate::{AnalysisError, AnalysisResult, AnalysisToken, AnalyzedText, FilteredText};

pub(super) fn tokenize(
    tokenizer: &Tokenizer,
    input: &FilteredText<'_>,
) -> AnalysisResult<AnalyzedText> {
    let text = input.as_str();
    let mut tokens = Vec::new();
    match tokenizer {
        Tokenizer::Whitespace => {
            for range in word_ranges(text) {
                tokens.push(AnalysisToken::from_source(input, range)?);
            }
        }
        Tokenizer::Standard | Tokenizer::Letter => {
            let expression = if matches!(tokenizer, Tokenizer::Standard) {
                standard_word_re()?
            } else {
                letter_re()?
            };
            for matched in expression.find_iter(text) {
                tokens.push(AnalysisToken::from_source(input, matched.range())?);
            }
        }
        Tokenizer::NGram { min_gram, max_gram } => {
            validate_gram_bounds("n-gram tokenizer", *min_gram, *max_gram)?;
            for word in word_ranges(text) {
                let boundaries: Vec<_> = text[word.clone()]
                    .char_indices()
                    .map(|(offset, _)| word.start + offset)
                    .chain(std::iter::once(word.end))
                    .collect();
                let length = boundaries.len() - 1;
                for n in *min_gram..=(*max_gram).min(length) {
                    for start in 0..=length - n {
                        tokens.push(AnalysisToken::from_source(
                            input,
                            boundaries[start]..boundaries[start + n],
                        )?);
                    }
                }
            }
        }
        Tokenizer::Pattern { pattern } => {
            let expression = Regex::new(pattern).map_err(|source| AnalysisError::InvalidRegex {
                component: "pattern tokenizer",
                pattern: pattern.clone(),
                source,
            })?;
            let mut start = 0;
            for separator in expression.find_iter(text) {
                if start < separator.start() {
                    tokens.push(AnalysisToken::from_source(input, start..separator.start())?);
                }
                start = separator.end();
            }
            if start < text.len() {
                tokens.push(AnalysisToken::from_source(input, start..text.len())?);
            }
        }
        Tokenizer::Keyword => {
            if !text.is_empty() {
                tokens.push(AnalysisToken::from_source(input, 0..text.len())?);
            }
        }
    }
    AnalyzedText::from_source(tokens, input)
}

fn word_ranges(text: &str) -> Vec<Range<usize>> {
    let mut words = Vec::new();
    let mut start = None;
    for (offset, character) in text.char_indices() {
        if character.is_whitespace() {
            if let Some(start) = start.take() {
                words.push(start..offset);
            }
        } else {
            start.get_or_insert(offset);
        }
    }
    if let Some(start) = start {
        words.push(start..text.len());
    }
    words
}
