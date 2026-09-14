//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Japanese compound alternatives, user segments and unknown unigrams preserve ordered graph output.

use uqa_core::memory::Budgeted;

use super::viterbi::State;
use super::word::{self, TokenWord};
use super::{KuromojiMode, KuromojiOrigin, KuromojiToken};
use crate::kuromoji::error::invalid;
use crate::morphology::lattice::{Node, WordId};
use crate::AnalysisResult;

struct Alternative {
    start: usize,
    end: usize,
    word: WordId,
}

/// Dictionary attributes are read only after alternatives have been deduplicated and emitted.
#[derive(Clone, Copy)]
pub(super) struct PendingToken {
    pub word: TokenWord,
    pub start: usize,
    pub end: usize,
    pub length: u32,
    pub order: usize,
}

pub(super) fn backtrace(state: &mut State<'_>, end: usize, mut index: usize) -> AnalysisResult<()> {
    if end == state.traversal.last_backtrace {
        return Ok(());
    }
    let mut position = end;
    let mut alternative: Option<Alternative> = None;
    let mut last_left = None;
    let mut back_count = 0_usize;
    while position > state.traversal.last_backtrace {
        state.tick()?;
        let mut node = state.traversal.lattice.get(position)[index];
        if state.options.mode != KuromojiMode::Normal
            && alternative.is_none()
            && !matches!(node.word, WordId::User(_))
        {
            let penalty = state.penalty(node.back_pos, position - node.back_pos)?;
            if penalty > 0 {
                let mut maximum = node.cost.wrapping_add(penalty);
                if let Some(left) = last_left {
                    let right = word::costs(node.word, state.model, state.user).right;
                    maximum = maximum.wrapping_add(i32::from(
                        state
                            .model
                            .connection_cost(right, left)
                            .expect("validated contexts"),
                    ));
                }
                super::resegment::prune(state, node.back_pos, position, node.back_index)?;
                let mut least = i32::MAX;
                let mut best = None;
                for candidate in 0..state.traversal.lattice.get(position).len() {
                    state.tick()?;
                    let choice = state.traversal.lattice.get(position)[candidate];
                    let mut cost = choice.cost;
                    if let Some(left) = last_left {
                        cost = cost.wrapping_add(i32::from(
                            state
                                .model
                                .connection_cost(choice.right, left)
                                .expect("validated contexts"),
                        ));
                    }
                    if cost < least {
                        least = cost;
                        best = Some(candidate);
                    }
                }
                if let Some(best) = best {
                    let choice = state.traversal.lattice.get(position)[best];
                    if least <= maximum && choice.back_pos != node.back_pos {
                        alternative = Some(Alternative {
                            start: node.back_pos,
                            end: position,
                            word: node.word,
                        });
                        node = choice;
                        back_count = 0;
                    }
                }
            }
        }
        if alternative
            .as_ref()
            .is_some_and(|alt| alt.start >= node.back_pos)
        {
            let alt = alternative.take().expect("alternate backtrace joins here");
            if !state.options.discard_compound_token && back_count > 0 {
                back_count += 1;
                let mut token = dictionary_token(alt.word, alt.start, alt.end)?;
                token.length = u32::try_from(back_count)
                    .map_err(|_| invalid("Kuromoji emission", "position length exceeds u32"))?;
                state.push(token)?;
            }
        }
        back_count += emit_word(state, node, position)?;
        last_left = Some(word::costs(node.word, state.model, state.user).left);
        position = node.back_pos;
        index = node.back_index;
    }
    state.traversal.last_backtrace = end;
    state
        .traversal
        .lattice
        .release_before(end, state.traversal.poll)?;
    Ok(())
}

fn emit_word(state: &mut State<'_>, node: Node, position: usize) -> AnalysisResult<usize> {
    let word_start = node.word_pos;
    let mut emitted = 0;
    match node.word {
        WordId::User(phrase) => {
            let entry = state
                .user
                .expect("selected user model")
                .entry(phrase)
                .expect("matched phrase");
            let first_pending = state.pending.len();
            let mut current = word_start;
            for (segment, &length) in entry.segment_lengths().iter().enumerate() {
                state.tick()?;
                let next = current + length;
                let token = token(
                    TokenWord::UserSegment(entry.word_base() + segment as u32),
                    current,
                    next,
                );
                state.push(token)?;
                current = next;
            }
            let mut left = first_pending;
            let mut right = state.pending.len();
            while left < right {
                state.tick()?;
                right -= 1;
                state.pending.swap(left, right);
                left += 1;
            }
            emitted += entry.segment_lengths().len();
        }
        WordId::Unknown(_) if state.options.mode == KuromojiMode::Extended => {
            let mut cursor = position;
            while cursor > word_start {
                state.tick()?;
                let mut start = cursor - 1;
                if start > word_start && (0xdc00..=0xdfff).contains(&state.input[start]) {
                    start -= 1;
                }
                if !state.options.discard_punctuation || !state.punctuation(state.input[start]) {
                    let token = token(
                        TokenWord::Dictionary(
                            state.ngram.expect("validated NGRAM word"),
                            KuromojiOrigin::Unknown,
                        ),
                        start,
                        cursor,
                    );
                    state.push(token)?;
                    emitted += 1;
                }
                cursor = start;
            }
        }
        word => {
            if !state.options.discard_punctuation
                || position == word_start
                || !state.punctuation(state.input[word_start])
            {
                let token = dictionary_token(word, word_start, position)?;
                state.push(token)?;
                emitted += 1;
            }
        }
    }
    Ok(emitted)
}

pub(super) fn dictionary_token(
    word: WordId,
    start: usize,
    end: usize,
) -> AnalysisResult<PendingToken> {
    let word = match word {
        WordId::Known(id) => TokenWord::Dictionary(id, KuromojiOrigin::Known),
        WordId::Unknown(id) => TokenWord::Dictionary(id, KuromojiOrigin::Unknown),
        WordId::User(_) => {
            return Err(invalid("Kuromoji emission", "user phrase requires segmentation").into())
        }
    };
    Ok(token(word, start, end))
}

pub(super) fn token(word: TokenWord, start: usize, end: usize) -> PendingToken {
    PendingToken {
        word,
        start,
        end,
        length: 1,
        order: 0,
    }
}

pub(super) fn materialize(
    state: &mut State<'_>,
    pending: PendingToken,
) -> AnalysisResult<Budgeted<KuromojiToken>> {
    let PendingToken {
        word,
        start,
        end,
        length,
        ..
    } = pending;
    let surface = state
        .input
        .get(start..end)
        .ok_or_else(|| invalid("Kuromoji emission", "token exceeds its source"))?;
    let (attributes, errors) = word.attributes(state.model, state.user);
    if !state.defer_attributes {
        errors.validate()?;
    }
    let mut units = surface.len();
    for attribute in attributes.into_iter().flatten() {
        let count =
            crate::allocation::input::utf16_len(attribute, state.traversal.poll, |_| Ok(()))?;
        units = units
            .checked_add(count)
            .ok_or_else(|| invalid("Kuromoji emission", "attribute size overflow"))?;
        state.check_units(units)?;
    }
    state.check_units(units)?;
    let (term_utf16, mut memory) =
        crate::allocation::copy_units(surface, state.budget, state.traversal.poll)?.into_parts();
    let mut owned = std::array::from_fn::<_, 6, _>(|_| None);
    for (index, attribute) in attributes.into_iter().enumerate() {
        if let Some(attribute) = attribute {
            let (text, allocation) =
                crate::allocation::copy_text(attribute, state.budget, state.traversal.poll)?
                    .into_parts();
            memory.absorb(allocation);
            owned[index] = Some(text);
        }
    }
    let [part_of_speech, base_form, reading, pronunciation, inflection_type, inflection_form] =
        owned;
    state.record_units(units)?;
    Ok(Budgeted::new(
        KuromojiToken {
            term_utf16,
            start_utf16: start,
            end_utf16: end,
            position_increment: 1,
            position_length: length,
            keyword: false,
            part_of_speech,
            base_form,
            reading,
            pronunciation,
            inflection_type,
            inflection_form,
            origin: Some(word.origin()),
            errors,
        },
        memory,
    ))
}
