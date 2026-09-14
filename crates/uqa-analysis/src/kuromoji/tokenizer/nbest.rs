//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Cost-bounded Japanese alternatives, stable span deduplication and graph edge fixups.

use uqa_core::memory::BudgetedVec;
use uqa_core::ordering::sort_by_with_control;

use super::{emission, viterbi::State, word::TokenWord, KuromojiOrigin};
use crate::kuromoji::error::{check_limit, invalid};
use crate::morphology::lattice::WordId;
use crate::AnalysisResult;

mod examples;
mod lattice;
pub(super) use lattice::Graph;

pub(super) fn count_work(work: &mut usize, limit: usize) -> AnalysisResult<()> {
    *work = work
        .checked_add(1)
        .ok_or_else(|| invalid("Kuromoji N-best", "work count overflow"))?;
    check_limit("Kuromoji N-best work", *work, limit)?;
    Ok(())
}

pub(super) fn backtrace(state: &mut State<'_>, position: usize, eos: bool) -> AnalysisResult<()> {
    let mut graph = state
        .nbest
        .take()
        .unwrap_or_else(|| Graph::new(state.budget));
    graph.setup(state, position, eos)?;
    graph.register(state)?;
    state.nbest = Some(graph);
    Ok(())
}

pub(super) fn probe_delta(
    state: &mut State<'_>,
    range: std::ops::Range<usize>,
) -> AnalysisResult<i32> {
    let graph = state.nbest.take().expect("prepared probe lattice");
    let delta = graph.probe_delta(state, range)?;
    state.nbest = Some(graph);
    Ok(delta)
}

fn register_node(
    state: &mut State<'_>,
    word: WordId,
    start: usize,
    end: usize,
) -> AnalysisResult<()> {
    if state.options.discard_punctuation && state.punctuation(state.input[start]) {
        return Ok(());
    }
    if let WordId::User(phrase) = word {
        let entry = state
            .user
            .expect("selected user model")
            .entry(phrase)
            .expect("matched phrase");
        state.push(emission::token(
            TokenWord::UserSegment(entry.word_base()),
            start,
            end,
        ))?;
        let mut current = start;
        for (index, &length) in entry.segment_lengths().iter().enumerate() {
            state.n_best_tick()?;
            if length < end - start {
                state.push(emission::token(
                    TokenWord::UserSegment(entry.word_base() + index as u32),
                    current,
                    current + length,
                ))?;
            }
            current += length;
        }
    } else {
        state.push(emission::dictionary_token(word, start, end)?)?;
    }
    Ok(())
}

fn origin(origin: KuromojiOrigin) -> u8 {
    match origin {
        KuromojiOrigin::Known => 0,
        KuromojiOrigin::Unknown => 1,
        KuromojiOrigin::User => 2,
    }
}

pub(super) fn fixup(state: &mut State<'_>) -> AnalysisResult<()> {
    {
        let work = &mut state.n_best_work;
        let limit = state.limits.max_n_best_work;
        let poll = &mut state.traversal.poll;
        sort_by_with_control(
            &mut state.pending,
            &mut || {
                count_work(work, limit)?;
                poll()
            },
            |a, b, _| {
                Ok(a.start
                    .cmp(&b.start)
                    .then_with(|| (a.end - a.start).cmp(&(b.end - b.start)))
                    .then_with(|| origin(b.word.origin()).cmp(&origin(a.word.origin())))
                    .then_with(|| a.order.cmp(&b.order)))
            },
        )?;
    }
    let mut retained = 0;
    for index in 0..state.pending.len() {
        state.n_best_tick()?;
        let token = state.pending[index];
        if retained == 0
            || state.pending[retained - 1].start != token.start
            || state.pending[retained - 1].end != token.end
        {
            state.pending[retained] = token;
            retained += 1;
        }
    }
    state.pending.truncate(retained);
    let mut offsets = BudgetedVec::new(state.budget);
    for index in 0..state.pending.len() {
        state.n_best_tick()?;
        offsets.push(state.pending[index].start)?;
        offsets.push(state.pending[index].end)?;
    }
    {
        let work = &mut state.n_best_work;
        let limit = state.limits.max_n_best_work;
        let poll = &mut state.traversal.poll;
        sort_by_with_control(
            &mut offsets,
            &mut || {
                count_work(work, limit)?;
                poll()
            },
            |a, b, _| Ok(a.cmp(b)),
        )?;
    }
    let mut unique = 0;
    for index in 0..offsets.len() {
        state.n_best_tick()?;
        if unique == 0 || offsets[unique - 1] != offsets[index] {
            offsets[unique] = offsets[index];
            unique += 1;
        }
    }
    offsets.truncate(unique);
    for index in 0..state.pending.len() {
        state.n_best_tick()?;
        let token = &mut state.pending[index];
        let start = offsets
            .binary_search(&token.start)
            .expect("retained token start");
        let end = offsets
            .binary_search(&token.end)
            .expect("retained token end");
        token.length = u32::try_from(end - start)
            .map_err(|_| invalid("Kuromoji N-best", "position length exceeds u32"))?;
    }
    for left in 0..state.pending.len() / 2 {
        state.n_best_tick()?;
        let right = state.pending.len() - 1 - left;
        state.pending.swap(left, right);
    }
    Ok(())
}

#[cfg(test)]
mod tests;
