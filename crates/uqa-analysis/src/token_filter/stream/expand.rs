//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Expansion transfers input leases and retains every output copy until the result is dropped.

use uqa_core::memory::{Budgeted, MemoryError};

use super::super::compiled::PreparedSynonyms;
use crate::token::{
    allocation::{TokenBatchAllocation, TokenBuffer},
    TokenBatch,
};
use crate::{AnalysisError, AnalysisResult, AnalysisToken, TokenTerm};

fn alternatives<'a>(
    token: &AnalysisToken,
    synonyms: &'a PreparedSynonyms<'_>,
    poll: &mut dyn FnMut() -> AnalysisResult<()>,
) -> AnalysisResult<Option<&'a [String]>> {
    if let Some(term) = token.term.as_str() {
        synonyms.get(term, poll)
    } else {
        poll()?;
        Ok(None)
    }
}

fn with_increment(token: Budgeted<AnalysisToken>, increment: u32) -> Budgeted<AnalysisToken> {
    let (mut token, memory) = token.into_parts();
    token.position_increment = increment;
    Budgeted::new(token, memory)
}

fn add_increment(left: u32, right: u32) -> AnalysisResult<u32> {
    left.checked_add(right)
        .ok_or(AnalysisError::TokenPositionOverflow)
}

pub(super) fn synonyms(
    input: TokenBatchAllocation,
    synonyms: &PreparedSynonyms<'_>,
    poll: &mut dyn FnMut() -> AnalysisResult<()>,
) -> AnalysisResult<Budgeted<TokenBatch>> {
    poll()?;
    let mut count = input.tokens().len();
    for token in input.tokens() {
        count = count
            .checked_add(alternatives(token, synonyms, poll)?.map_or(0, <[String]>::len))
            .ok_or(MemoryError::SizeOverflow)?;
    }
    if count == input.tokens().len() {
        return input.finish(poll);
    }
    let budget = input.budget().clone();
    let mut output = TokenBuffer::new(&budget);
    output.reserve_tokens(count)?;
    let mut input = input.into_input();
    loop {
        let (token, remaining) = input.next(poll)?;
        input = remaining;
        let Some(token) = token else { break };
        let alternatives = alternatives(&token, synonyms, poll)?;
        let original = output.len();
        output.push(token)?;
        for term in alternatives.into_iter().flatten() {
            let (term, memory) = crate::allocation::copy_text(term, &budget, poll)?.into_parts();
            let alternative = output.token(original).rewrite_budgeted(
                Budgeted::new(TokenTerm::from(term), memory),
                &budget,
                poll,
            )?;
            output.push(with_increment(alternative, 0))?;
        }
    }
    let (terminal, final_increment) = input.finish();
    if let Some(terminal) = terminal {
        output.set_terminal_box(terminal);
    }
    output.finish_batch(final_increment, poll)
}

fn gram_count(length: usize, minimum: usize, maximum: usize, edge: bool) -> AnalysisResult<usize> {
    let maximum = maximum.min(length);
    let count = maximum - minimum + 1;
    if edge {
        return Ok(count);
    }
    let ends = (length - minimum + 1)
        .checked_add(length - maximum + 1)
        .ok_or(MemoryError::SizeOverflow)?;
    let (left, right) = if count.is_multiple_of(2) {
        (count / 2, ends)
    } else {
        (count, ends / 2)
    };
    Ok(left.checked_mul(right).ok_or(MemoryError::SizeOverflow)?)
}

pub(super) fn grams(
    input: TokenBatchAllocation,
    minimum: usize,
    maximum: usize,
    keep_short: bool,
    edge: bool,
    poll: &mut dyn FnMut() -> AnalysisResult<()>,
) -> AnalysisResult<Budgeted<TokenBatch>> {
    poll()?;
    let mut count = 0usize;
    let mut passthrough = true;
    for token in input.tokens() {
        let length = token.term.character_count_with_control(poll)?;
        let emitted = if length < minimum {
            passthrough &= keep_short;
            usize::from(keep_short)
        } else {
            passthrough &= length == minimum;
            gram_count(length, minimum, maximum, edge)?
        };
        count = count
            .checked_add(emitted)
            .ok_or(MemoryError::SizeOverflow)?;
    }
    if passthrough {
        return input.finish(poll);
    }
    let budget = input.budget().clone();
    let mut output = TokenBuffer::new(&budget);
    output.reserve_tokens(count)?;
    let mut input = input.into_input();
    let mut skipped = 0;
    let mut trailing_removed = None;
    loop {
        let (token, remaining) = input.next(poll)?;
        input = remaining;
        let Some(token) = token else { break };
        let length = token.term.character_count_with_control(poll)?;
        if length < minimum {
            if keep_short {
                trailing_removed = None;
                let increment = add_increment(token.position_increment, skipped)?;
                skipped = 0;
                output.push(with_increment(token, increment))?;
            } else {
                skipped = add_increment(skipped, token.position_increment)?;
                trailing_removed = Some(token);
            }
            continue;
        }
        trailing_removed = None;
        if length == minimum {
            let increment = add_increment(token.position_increment, skipped)?;
            skipped = 0;
            output.push(with_increment(token, increment))?;
            continue;
        }
        let boundaries = token.term.boundaries_budgeted(&budget, poll)?;
        let mut first = true;
        for size in minimum..=maximum.min(length) {
            let last_start = if edge { 0 } else { length - size };
            for start in 0..=last_start {
                poll()?;
                let increment = if first {
                    first = false;
                    let increment = add_increment(token.position_increment, skipped)?;
                    skipped = 0;
                    increment
                } else {
                    0
                };
                let gram = token.substring_budgeted(
                    boundaries[start],
                    boundaries[start + size],
                    boundaries[length],
                    &budget,
                    poll,
                )?;
                output.push(with_increment(gram, increment))?;
            }
        }
    }
    let (terminal, final_increment) = input.finish();
    let final_increment = add_increment(final_increment, skipped)?;
    match terminal {
        Some(terminal) => {
            drop(trailing_removed);
            output.set_terminal_box(terminal);
        }
        None => {
            if let Some(terminal) = trailing_removed {
                output.set_terminal_token(terminal)?;
            }
        }
    }
    output.finish_batch(final_increment, poll)
}
