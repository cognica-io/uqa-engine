//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Number composition retains lookahead, terminal attributes, and their unique allocation leases.

use crate::nori::error::{check_limit, invalid};
use crate::nori::filters::stream::{AllocatedStream, FilterToken};
use crate::nori::filters::{token_units, Work};
use crate::nori::NoriLimits;
use crate::token::allocation::{TokenBatchAllocation, TokenBatchInput, TokenBuffer};
use crate::{AnalysisError, AnalysisResult};
use uqa_core::memory::{Budgeted, BudgetedVec, MemoryBudget, MemoryReservation};

struct OwnedToken<T> {
    token: T,
    memory: MemoryReservation,
}

impl<T> From<Budgeted<T>> for OwnedToken<T> {
    fn from(input: Budgeted<T>) -> Self {
        let (token, memory) = input.into_parts();
        Self { token, memory }
    }
}

impl<T> OwnedToken<T> {
    fn into_budgeted(self) -> Budgeted<T> {
        Budgeted::new(self.token, self.memory)
    }
}

impl<T> std::ops::Deref for OwnedToken<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.token
    }
}

struct State<'a, T: FilterToken> {
    context: T::Context,
    input: Option<TokenBatchInput<T>>,
    current: Option<OwnedToken<T>>,
    changed: bool,
    saved: Option<OwnedToken<T>>,
    numeral: BudgetedVec<u16>,
    fall_through: u32,
    exhausted: bool,
    output: TokenBuffer<T>,
    output_units: usize,
    final_position_increment: u32,
    budget: MemoryBudget,
    limits: NoriLimits,
    work: Work<'a>,
}

pub(in crate::nori) fn filter<T: FilterToken>(
    input: AllocatedStream<T>,
    limits: NoriLimits,
    poll: &mut impl FnMut() -> AnalysisResult<()>,
) -> AnalysisResult<AllocatedStream<T>> {
    let work = Work::new(poll)?;
    check_limit(
        "Nori output tokens",
        input.batch.tokens().len(),
        limits.max_tokens,
    )?;
    check_limit(
        "Nori input UTF-16 units",
        input.final_offset_utf16,
        limits.max_input_utf16,
    )?;
    let budget = input.batch.budget().clone();
    let mut state = State {
        context: input.context,
        input: Some(input.batch.into_input()),
        current: None,
        changed: false,
        saved: None,
        numeral: BudgetedVec::new(&budget),
        fall_through: 0,
        exhausted: false,
        output: TokenBuffer::new(&budget),
        output_units: 0,
        final_position_increment: 0,
        budget,
        limits,
        work,
    };
    while state.next()? {
        let token = state.current.take().expect("emitted attributes");
        let length = token.term_len(&mut state.work)?;
        state.output_units = state.total_units(&token, length)?;
        check_limit(
            "Nori output tokens",
            state.output.len() + 1,
            limits.max_tokens,
        )?;
        state.output.push(token.into_budgeted())?;
        state.changed = false;
    }
    state.numeral = BudgetedVec::new(&state.budget);
    state.saved = None;
    if state.changed {
        if let Some(token) = state.current.take() {
            let length = token.term_len(&mut state.work)?;
            state.total_units(&token, length)?;
            state.output.set_terminal_token(token.into_budgeted())?;
        }
    }
    state.work.finish()?;
    Ok(AllocatedStream {
        context: state.context,
        batch: TokenBatchAllocation::from_budgeted(
            state.output.into_batch(state.final_position_increment),
        ),
        final_offset_utf16: input.final_offset_utf16,
    })
}

impl<T: FilterToken> State<'_, T> {
    fn total_units(&mut self, token: &T, term_units: usize) -> AnalysisResult<usize> {
        let total = self
            .output_units
            .checked_add(token_units(token, term_units, &mut self.work)?)
            .ok_or_else(|| invalid("Nori number", "attribute size overflow"))?;
        check_limit(
            "Nori output UTF-16 units",
            total,
            self.limits.max_output_utf16,
        )?;
        Ok(total)
    }

    fn read(&mut self) -> AnalysisResult<bool> {
        self.work.tick()?;
        let Some(input) = self.input.take() else {
            return Ok(false);
        };
        let (token, input) = input.next(self.work.poll)?;
        let (token, more) = if let Some(token) = token {
            self.input = Some(input);
            (Some(token), true)
        } else {
            let (terminal, increment) = input.finish();
            self.final_position_increment = increment;
            let token = terminal.map(|terminal| {
                let (terminal, mut memory) = terminal.into_parts();
                let token = {
                    let allocation = terminal;
                    *allocation
                };
                drop(memory.split(size_of::<T>()));
                Budgeted::new(token, memory)
            });
            (token, false)
        };
        if let Some(token) = token {
            let length = token.term_len(&mut self.work)?;
            self.total_units(&token, length)?;
            self.current = Some(token.into());
            self.changed = true;
        }
        Ok(more)
    }

    fn next(&mut self) -> AnalysisResult<bool> {
        self.work.tick()?;
        if let Some(saved) = self.saved.take() {
            self.current = Some(saved);
            self.changed = true;
            return Ok(true);
        }
        if self.exhausted {
            return Ok(false);
        }
        if !self.read()? {
            self.exhausted = true;
            return Ok(false);
        }
        let current = self.current.as_ref().expect("read attributes");
        if current.keyword() {
            return Ok(true);
        }
        if self.fall_through > 0 {
            self.fall_through -= 1;
            return Ok(true);
        }
        if current.increment() == 0 {
            self.fall_through = current
                .position_length()
                .checked_sub(1)
                .ok_or(AnalysisError::InvalidTokenPosition)?;
            return Ok(true);
        }
        if !super::numeral(current.term(), &mut self.work)? {
            return Ok(true);
        }
        self.compose()
    }

    fn compose(&mut self) -> AnalysisResult<bool> {
        let original = self
            .current
            .as_ref()
            .expect("numeric attributes")
            .clone_reserved(&self.budget, &mut self.work)?;
        let mut term = original.copy_term(&self.budget, &mut self.work)?;
        let first = original.span();
        let mut last;
        let more = loop {
            self.work.tick()?;
            last = self.current.as_ref().expect("numeric attributes").span();
            let more = self.read()?;
            if !more {
                self.exhausted = true;
            }
            let current = self
                .current
                .as_ref()
                .expect("last successful or explicit terminal attributes");
            if current.increment() == 0 {
                self.fall_through = current
                    .position_length()
                    .checked_sub(1)
                    .ok_or(AnalysisError::InvalidTokenPosition)?;
                self.saved = Some(current.clone_reserved(&self.budget, &mut self.work)?.into());
                self.current = Some(original.into());
                self.changed = true;
                // A numeral prefix survives an aborted composition, matching shared lookahead state.
                return Ok(more);
            }
            let required = self
                .numeral
                .len()
                .checked_add(term.len())
                .ok_or_else(|| invalid("Nori number", "numeral size overflow"))?;
            check_limit("Nori numeric units", required, self.limits.max_output_utf16)?;
            self.numeral.reserve(term.len())?;
            for unit in term.iter() {
                self.work.tick()?;
                self.numeral.push(*unit)?;
            }
            if !more {
                break false;
            }
            term.clear();
            term.reserve(current.term_len(&mut self.work)?)?;
            for unit in current.term() {
                self.work.tick()?;
                term.push(unit)?;
            }
            if !super::numeral(term.iter().copied(), &mut self.work)?
                && !super::punctuation(term.iter().copied(), &mut self.work)?
            {
                break true;
            }
        };
        drop(original);
        drop(term);
        if more {
            self.saved = Some(
                self.current
                    .as_ref()
                    .expect("lookahead attributes")
                    .clone_reserved(&self.budget, &mut self.work)?
                    .into(),
            );
        }
        let mut current = self.current.take().expect("composed attributes");
        current
            .token
            .cover(&first, &last, &self.context, &mut self.work)?;
        let metadata = self.total_units(&current, 0)?;
        let maximum = self.limits.max_output_utf16 - metadata;
        let numeral = std::mem::replace(&mut self.numeral, BudgetedVec::new(&self.budget));
        let normalized =
            super::normalize_budgeted(&numeral, maximum, &self.budget, &mut self.work)?;
        current.token.replace_term(
            normalized,
            &mut current.memory,
            &self.context,
            &mut self.work,
        )?;
        self.current = Some(current);
        self.changed = true;
        Ok(true)
    }
}
