//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native morphology streams share source projection, graph validation and allocation transfer.

use std::ops::Range;

use uqa_core::memory::{Budgeted, MemoryReservation};

use super::{allocation::TokenBuffer, AnalysisToken, AnalyzedText, Morphology, TokenBatch};
use crate::{AnalysisError, AnalysisResult, FilteredText, TokenTerm};

pub(super) struct NativeFields {
    pub span: Range<usize>,
    pub increment: u32,
    pub length: u32,
    pub keyword: bool,
    pub morphology: Morphology,
}

pub(super) trait NativeToken {
    fn take_term(&mut self) -> Vec<u16>;
    fn into_fields(self) -> NativeFields;
}

fn attach_source(
    term: TokenTerm,
    term_utf16_len: usize,
    fields: NativeFields,
    input: &FilteredText<'_>,
) -> AnalysisResult<AnalysisToken> {
    let offsets = input.source_covering_offsets_utf16(fields.span.clone())?;
    let verbatim = term.as_str().is_some_and(|text| {
        input.original().get(offsets.utf8.clone()) == Some(text)
            && term_utf16_len == offsets.utf16.len()
    });
    Ok(AnalysisToken {
        term,
        offsets: Some(offsets),
        position_increment: fields.increment,
        position_length: fields.length,
        keyword: fields.keyword,
        filtered_utf16: Some(fields.span),
        morphology: Some(fields.morphology),
        verbatim,
    })
}

pub(super) fn token<T: NativeToken>(
    mut token: T,
    input: &FilteredText<'_>,
) -> AnalysisResult<AnalysisToken> {
    let units = token.take_term();
    let length = units.len();
    attach_source(
        TokenTerm::from_utf16(units),
        length,
        token.into_fields(),
        input,
    )
}

pub(super) fn output<T: NativeToken>(
    batch: TokenBatch<T>,
    final_offset_utf16: usize,
    input: &FilteredText<'_>,
) -> AnalysisResult<AnalyzedText> {
    let length = input.as_str().encode_utf16().count();
    if final_offset_utf16 != length {
        return Err(AnalysisError::MismatchedAnalysisInput {
            expected_utf16: final_offset_utf16,
            actual_utf16: length,
        });
    }
    let batch = TokenBatch {
        tokens: batch
            .tokens
            .into_iter()
            .map(|value| token(value, input))
            .collect::<AnalysisResult<_>>()?,
        final_position_increment: batch.final_position_increment,
        terminal: batch
            .terminal
            .map(|value| token(*value, input))
            .transpose()?
            .map(Box::new),
    };
    batch.validate_positions()?;
    Ok(AnalyzedText {
        batch,
        final_offsets: input.final_offsets(),
        projection: input.projection(),
    })
}

pub(super) fn output_budgeted<T: NativeToken>(
    batch: TokenBatch<T>,
    final_offset_utf16: usize,
    memory: MemoryReservation,
    input: &FilteredText<'_>,
    poll: &mut dyn FnMut() -> AnalysisResult<()>,
) -> AnalysisResult<Budgeted<AnalyzedText>> {
    let mut buffer = TokenBuffer::new(memory.budget());
    buffer.memory.absorb(memory);
    let TokenBatch {
        tokens,
        terminal,
        final_position_increment,
    } = batch;
    poll()?;
    let length = input.filtered_utf16(0..input.as_str().len())?.end;
    if final_offset_utf16 != length {
        return Err(AnalysisError::MismatchedAnalysisInput {
            expected_utf16: final_offset_utf16,
            actual_utf16: length,
        });
    }
    let original_buffer_bytes = tokens.capacity() * size_of::<T>();
    let mut pending = tokens.into_iter();
    for token in pending.by_ref() {
        poll()?;
        let token = convert(&mut buffer, token, input, poll)?;
        buffer.tokens.push(token)?;
    }
    drop(pending);
    drop(buffer.memory.split(original_buffer_bytes));
    if let Some(terminal) = terminal {
        let token = {
            let allocation = terminal;
            *allocation
        };
        drop(buffer.memory.split(size_of::<T>()));
        let token = convert(&mut buffer, token, input, poll)?;
        buffer.set_terminal(token)?;
    }
    buffer.finish(input, final_position_increment, poll)
}

fn convert<T: NativeToken>(
    buffer: &mut TokenBuffer,
    mut token: T,
    input: &FilteredText<'_>,
    poll: &mut dyn FnMut() -> AnalysisResult<()>,
) -> AnalysisResult<AnalysisToken> {
    let units = token.take_term();
    let length = units.len();
    let memory = buffer.memory.split(units.capacity() * size_of::<u16>());
    let term = TokenTerm::from_utf16_budgeted(Budgeted::new(units, memory), &mut *poll)?;
    let (term, memory) = term.into_parts();
    buffer.memory.absorb(memory);
    attach_source(term, length, token.into_fields(), input)
}
