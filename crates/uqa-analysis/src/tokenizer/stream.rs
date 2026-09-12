//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Tokenizer emission with original source spans.

use std::ops::Range;

use super::PreparedTokenizer;
use crate::{AnalysisResult, AnalysisToken, AnalyzedText, FilteredText};

pub(super) fn tokenize(
    tokenizer: &PreparedTokenizer,
    input: &FilteredText<'_>,
) -> AnalysisResult<AnalyzedText> {
    let text = input.as_str();
    let mut tokens = Vec::new();
    match tokenizer {
        #[cfg(feature = "nori")]
        PreparedTokenizer::Nori(tokenizer) => {
            return tokenizer.tokenize(text)?.into_analyzed(input)
        }
        PreparedTokenizer::Whitespace => {
            for range in word_ranges(text) {
                tokens.push(AnalysisToken::from_source(input, range)?);
            }
        }
        PreparedTokenizer::Matches(expression) => {
            for matched in expression.find_iter(text) {
                tokens.push(AnalysisToken::from_source(input, matched.range())?);
            }
        }
        PreparedTokenizer::NGram { min_gram, max_gram } => {
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
        PreparedTokenizer::Pattern(expression) => {
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
        PreparedTokenizer::Keyword => {
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
