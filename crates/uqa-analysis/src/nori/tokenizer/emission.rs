//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Backward token emission, with compound alternatives and exact reference offsets.

use uqa_core::memory::Budgeted;

use super::allocation;
use super::lattice::WordId;
use super::viterbi::{punctuation, State};
use super::word::Word;
use super::{DecompoundMode, NoriToken};
use crate::nori::error::invalid;
use crate::nori::POSType;
use crate::AnalysisResult;

pub(super) fn backtrace(state: &mut State<'_>, end: usize, mut index: usize) -> AnalysisResult<()> {
    if end == state.last_backtrace {
        return Ok(());
    }
    let mut position = end;
    while position > state.last_backtrace {
        state.tick()?;
        let node = state.lattice.get(position)[index];
        if state.options.output_unknown_unigrams && matches!(node.word, WordId::Unknown(_)) {
            let mut cursor = position;
            while cursor > node.word_pos {
                state.tick()?;
                let mut start = cursor - 1;
                if start > node.word_pos && (0xdc00..=0xdfff).contains(&state.input[start]) {
                    start -= 1;
                }
                let token = token(
                    state,
                    WordId::Unknown(state.ngram.expect("validated NGRAM word")),
                    start,
                    cursor,
                )?;
                state.push(token)?;
                cursor = start;
            }
        } else {
            let token = token(state, node.word, node.word_pos, position)?;
            emit_word(state, token)?;
        }
        if !state.options.discard_punctuation && node.word_pos != node.back_pos {
            let id = state
                .model
                .unknown_words(state.model.character_class(u16::from(b' ')))
                .expect("SPACE words")
                .start;
            let token = token(state, WordId::Unknown(id), node.back_pos, node.word_pos)?;
            state.push(token)?;
        }
        position = node.back_pos;
        index = node.back_index;
    }
    state.last_backtrace = end;
    state.lattice.release_before(end, state.poll)?;
    Ok(())
}

fn token(
    state: &mut State<'_>,
    id: WordId,
    start: usize,
    end: usize,
) -> AnalysisResult<Budgeted<NoriToken>> {
    let word = Word::resolve(id, state.model, state.user);
    let surface = &state.input[start..end];
    let mut units = surface.len();
    if let Some(reading) = word.reading() {
        units = units
            .checked_add(allocation::utf16_len(reading, usize::MAX, state.poll)?)
            .ok_or_else(|| invalid("Nori emission", "reading size overflow"))?;
    }
    match &word {
        Word::Dictionary(word, _) => {
            for part in word.morphemes().into_iter().flatten() {
                state.tick()?;
                units = units
                    .checked_add(allocation::utf16_len(part.surface, usize::MAX, state.poll)?)
                    .ok_or_else(|| invalid("Nori emission", "morpheme size overflow"))?;
                state.check_units(units)?;
            }
        }
        Word::User(word) => {
            for length in word.segment_lengths().into_iter().flatten() {
                state.tick()?;
                units = units
                    .checked_add(*length)
                    .ok_or_else(|| invalid("Nori emission", "user morpheme size overflow"))?;
            }
        }
    }
    state.check_units(units)?;
    let mut memory = state.budget.empty_reservation();
    let (term_utf16, term_memory) =
        allocation::copy_units(surface, state.budget, state.poll)?.into_parts();
    memory.absorb(term_memory);
    let reading = word
        .reading()
        .map(|text| allocation::copy_string(text, state.budget, state.poll))
        .transpose()?;
    let reading = reading.map(|reading| {
        let (text, reading_memory) = reading.into_parts();
        memory.absorb(reading_memory);
        text
    });
    let (morphemes, morpheme_memory) = word
        .morphemes(surface, state.budget, state.poll)?
        .into_parts();
    memory.absorb(morpheme_memory);
    Ok(Budgeted::new(
        NoriToken {
            term_utf16,
            start_utf16: start,
            end_utf16: end,
            position_increment: 1,
            position_length: 1,
            keyword: false,
            pos_type: word.pos_type(),
            left_pos: word.left_pos(),
            right_pos: word.right_pos(),
            reading,
            morphemes,
            origin: word.origin(),
        },
        memory,
    ))
}

fn emit_word(state: &mut State<'_>, original: Budgeted<NoriToken>) -> AnalysisResult<()> {
    if original.pos_type == POSType::Morpheme
        || state.options.decompound_mode == DecompoundMode::None
    {
        let unit = original.term_utf16[0];
        let category = state
            .model
            .unicode(u32::from(unit))
            .expect("complete Unicode table")
            .category;
        if !state.options.discard_punctuation || !punctuation(unit, category) {
            state.push(original)?;
        }
        return Ok(());
    }
    let Some(parts) = &original.morphemes else {
        return state.push(original);
    };
    let mut end = original.end_utf16;
    for (index, part) in parts.iter().enumerate().rev() {
        state.tick()?;
        let (start, token_end) = if original.pos_type == POSType::Compound {
            (
                end.checked_sub(part.surface_utf16.len())
                    .ok_or_else(|| invalid("Nori emission", "compound offset precedes input"))?,
                end,
            )
        } else {
            (original.start_utf16, original.end_utf16)
        };
        let (term_utf16, memory) =
            allocation::copy_units(&part.surface_utf16, state.budget, state.poll)?.into_parts();
        state.push(Budgeted::new(
            NoriToken {
                term_utf16,
                start_utf16: start,
                end_utf16: token_end,
                position_increment: u32::from(
                    index != 0 || state.options.decompound_mode != DecompoundMode::Mixed,
                ),
                position_length: 1,
                keyword: original.keyword,
                pos_type: POSType::Morpheme,
                left_pos: part.pos,
                right_pos: part.pos,
                reading: None,
                morphemes: None,
                origin: original.origin,
            },
            memory,
        ))?;
        // Only compound offsets use this value; inflection strings may exceed their source length.
        if original.pos_type == POSType::Compound {
            end = start;
        }
    }
    if state.options.decompound_mode == DecompoundMode::Mixed {
        let position_length = u32::try_from(parts.len().max(1))
            .map_err(|_| invalid("Nori emission", "position length exceeds u32"))?;
        let (mut original, memory) = original.into_parts();
        original.position_length = position_length;
        state.push(Budgeted::new(original, memory))?;
    }
    Ok(())
}
