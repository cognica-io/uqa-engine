//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Metadata-preserving implementations shared by rich and term-only filtering.

use std::collections::BTreeSet;

use super::{ascii_fold, builtin_stop_words, validate_gram_bounds, TokenFilter};
use crate::token::TokenBatch;
use crate::{porter, AnalysisError, AnalysisResult, AnalysisToken};

pub(super) fn filter(filter: &TokenFilter, mut batch: TokenBatch) -> AnalysisResult<TokenBatch> {
    match filter {
        TokenFilter::Lowercase | TokenFilter::ASCIIFolding | TokenFilter::PorterStem => {
            for token in &mut batch.tokens {
                let term = match filter {
                    TokenFilter::Lowercase => token.term.to_lowercase(),
                    TokenFilter::ASCIIFolding => ascii_fold(&token.term),
                    _ if token.keyword => continue,
                    _ => porter::stem(&token.term),
                };
                token.replace_term(term);
            }
        }
        TokenFilter::Stop {
            language,
            custom_words,
        } => {
            let mut words: BTreeSet<&str> = builtin_stop_words(language).iter().copied().collect();
            words.extend(custom_words.iter().map(String::as_str));
            batch = retain(batch, |token| !words.contains(token.term.as_str()))?;
        }
        TokenFilter::Synonym {
            synonyms,
            synonyms_path,
        } => {
            let resolved = if let Some(path) = synonyms_path {
                TokenFilter::parse_synonym_file(path)?
            } else {
                synonyms.clone()
            };
            let mut expanded = Vec::new();
            for token in batch.tokens {
                let alternatives = resolved.get(&token.term);
                let start = expanded.len();
                expanded.push(token);
                if let Some(alternatives) = alternatives {
                    for term in alternatives {
                        let mut alternative = expanded[start].clone();
                        alternative.replace_term(term.clone());
                        alternative.position_increment = 0;
                        expanded.push(alternative);
                    }
                }
            }
            batch.tokens = expanded;
        }
        TokenFilter::Ngram {
            min_gram,
            max_gram,
            keep_short,
        } => {
            validate_gram_bounds("n-gram token filter", *min_gram, *max_gram)?;
            batch = grams(batch, *min_gram, *max_gram, *keep_short, false)?;
        }
        TokenFilter::EdgeNgram { min_gram, max_gram } => {
            validate_gram_bounds("edge n-gram token filter", *min_gram, *max_gram)?;
            batch = grams(batch, *min_gram, *max_gram, false, true)?;
        }
        TokenFilter::Length {
            min_length,
            max_length,
        } => {
            batch = retain(batch, |token| {
                let length = token.term.chars().count();
                length >= *min_length && (*max_length == 0 || length <= *max_length)
            })?;
        }
    }
    batch.validate_positions()?;
    Ok(batch)
}

fn add_increment(left: u32, right: u32) -> AnalysisResult<u32> {
    left.checked_add(right)
        .ok_or(AnalysisError::TokenPositionOverflow)
}

fn retain(
    mut batch: TokenBatch,
    keep: impl Fn(&AnalysisToken) -> bool,
) -> AnalysisResult<TokenBatch> {
    let mut tokens = Vec::new();
    let mut skipped = 0;
    for mut token in batch.tokens {
        if keep(&token) {
            token.position_increment = add_increment(token.position_increment, skipped)?;
            skipped = 0;
            tokens.push(token);
        } else {
            skipped = add_increment(skipped, token.position_increment)?;
        }
    }
    batch.tokens = tokens;
    batch.final_position_increment = add_increment(batch.final_position_increment, skipped)?;
    Ok(batch)
}

fn grams(
    mut batch: TokenBatch,
    min_gram: usize,
    max_gram: usize,
    keep_short: bool,
    edge: bool,
) -> AnalysisResult<TokenBatch> {
    let mut tokens = Vec::new();
    let mut skipped = 0;
    for mut token in batch.tokens {
        let boundaries: Vec<_> = token
            .term
            .char_indices()
            .map(|(offset, _)| offset)
            .chain(std::iter::once(token.term.len()))
            .collect();
        let length = boundaries.len() - 1;
        if length < min_gram {
            if keep_short {
                token.position_increment = add_increment(token.position_increment, skipped)?;
                skipped = 0;
                tokens.push(token);
            } else {
                skipped = add_increment(skipped, token.position_increment)?;
            }
            continue;
        }
        let mut first = true;
        for n in min_gram..=max_gram.min(length) {
            let last_start = if edge { 0 } else { length - n };
            for start in 0..=last_start {
                let mut gram = token.substring(boundaries[start]..boundaries[start + n]);
                gram.position_increment = if first {
                    first = false;
                    let increment = add_increment(token.position_increment, skipped)?;
                    skipped = 0;
                    increment
                } else {
                    0
                };
                tokens.push(gram);
            }
        }
    }
    batch.tokens = tokens;
    batch.final_position_increment = add_increment(batch.final_position_increment, skipped)?;
    Ok(batch)
}
