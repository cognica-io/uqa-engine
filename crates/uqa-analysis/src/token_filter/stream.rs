//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Metadata-preserving implementations shared by rich and term-only filtering.

use super::{ascii_fold, PreparedTokenFilter};
use crate::token::TokenBatch;
use crate::{porter, AnalysisError, AnalysisResult, AnalysisToken, TokenTerm};

pub(super) fn filter(
    filter: &PreparedTokenFilter<'_>,
    mut batch: TokenBatch,
) -> AnalysisResult<TokenBatch> {
    match filter {
        PreparedTokenFilter::Lowercase
        | PreparedTokenFilter::ASCIIFolding
        | PreparedTokenFilter::PorterStem => {
            for token in &mut batch.tokens {
                let term = match filter {
                    PreparedTokenFilter::Lowercase => token.term.map_unicode(str::to_lowercase),
                    PreparedTokenFilter::ASCIIFolding => token.term.map_unicode(ascii_fold),
                    _ if token.keyword => continue,
                    _ => token.term.as_str().map_or_else(
                        || TokenTerm::from_utf16(porter::stem_utf16(&token.term.utf16())),
                        |text| TokenTerm::from(porter::stem(text)),
                    ),
                };
                token.replace_term(term);
            }
        }
        PreparedTokenFilter::Stop(words) => {
            batch = retain(batch, |token| {
                !token.term.as_str().is_some_and(|term| words.contains(term))
            })?;
        }
        PreparedTokenFilter::Synonym(resolved) => {
            let mut expanded = Vec::new();
            for token in batch.tokens {
                let alternatives = token.term.as_str().and_then(|term| resolved.get(term));
                let start = expanded.len();
                expanded.push(token);
                if let Some(alternatives) = alternatives {
                    for term in alternatives {
                        let mut alternative = expanded[start].clone();
                        alternative.replace_term(term.clone().into());
                        alternative.position_increment = 0;
                        expanded.push(alternative);
                    }
                }
            }
            batch.tokens = expanded;
        }
        PreparedTokenFilter::Ngram {
            min_gram,
            max_gram,
            keep_short,
        } => {
            batch = grams(batch, *min_gram, *max_gram, *keep_short, false)?;
        }
        PreparedTokenFilter::EdgeNgram { min_gram, max_gram } => {
            batch = grams(batch, *min_gram, *max_gram, false, true)?;
        }
        PreparedTokenFilter::Length {
            min_length,
            max_length,
        } => {
            batch = retain(batch, |token| {
                let length = token.term.character_count();
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
    let mut trailing_removed = None;
    for mut token in batch.tokens {
        if keep(&token) {
            trailing_removed = None;
            token.position_increment = add_increment(token.position_increment, skipped)?;
            skipped = 0;
            tokens.push(token);
        } else {
            skipped = add_increment(skipped, token.position_increment)?;
            trailing_removed = Some(token);
        }
    }
    batch.tokens = tokens;
    if batch.terminal.is_none() {
        batch.terminal = trailing_removed.map(Box::new);
    }
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
    let mut trailing_removed = None;
    for mut token in batch.tokens {
        let boundaries = token.term.boundaries();
        let length = boundaries.len() - 1;
        if length < min_gram {
            if keep_short {
                trailing_removed = None;
                token.position_increment = add_increment(token.position_increment, skipped)?;
                skipped = 0;
                tokens.push(token);
            } else {
                skipped = add_increment(skipped, token.position_increment)?;
                trailing_removed = Some(token);
            }
            continue;
        }
        trailing_removed = None;
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
    if batch.terminal.is_none() {
        batch.terminal = trailing_removed.map(Box::new);
    }
    batch.final_position_increment = add_increment(batch.final_position_increment, skipped)?;
    Ok(batch)
}
