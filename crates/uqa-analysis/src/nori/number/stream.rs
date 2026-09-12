//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Number composition retains Lucene's shared lookahead and terminal attributes.

use crate::nori::error::{check_limit, invalid};
use crate::nori::filters::stream::{FilterStream, FilterToken};
use crate::nori::filters::{token_units, Work};
use crate::nori::{DictionaryError, NoriLimits};
use crate::{AnalysisError, AnalysisResult};

struct State<'a, T: FilterToken> {
    context: T::Context,
    input: std::vec::IntoIter<T>,
    terminal: Option<Box<T>>,
    current: Option<T>,
    changed: bool,
    saved: Option<T>,
    numeral: Vec<u16>,
    fall_through: u32,
    exhausted: bool,
    output: Vec<T>,
    output_units: usize,
    limits: NoriLimits,
    work: Work<'a>,
}

pub(in crate::nori) fn filter<T: FilterToken>(
    input: FilterStream<T>,
    limits: NoriLimits,
    poll: &mut impl FnMut() -> AnalysisResult<()>,
) -> AnalysisResult<FilterStream<T>> {
    let work = Work::new(poll)?;
    check_limit("Nori output tokens", input.tokens.len(), limits.max_tokens)?;
    check_limit(
        "Nori input UTF-16 units",
        input.final_offset_utf16,
        limits.max_input_utf16,
    )?;
    let mut state = State {
        context: input.context,
        input: input.tokens.into_iter(),
        terminal: input.terminal,
        current: None,
        changed: false,
        saved: None,
        numeral: Vec::new(),
        fall_through: 0,
        exhausted: false,
        output: Vec::new(),
        output_units: 0,
        limits,
        work,
    };
    while state.next()? {
        let token = state.current.take().expect("emitted attributes");
        state.output_units = state.total_units(&token, token.term_len())?;
        check_limit(
            "Nori output tokens",
            state.output.len() + 1,
            limits.max_tokens,
        )?;
        state.output.try_reserve(1).map_err(DictionaryError::from)?;
        state.output.push(token);
        state.changed = false;
    }
    let terminal = if state.changed {
        state.current.take().map(Box::new)
    } else {
        None
    };
    if let Some(token) = &terminal {
        state.total_units(token, token.term_len())?;
    }
    state.work.finish()?;
    Ok(FilterStream {
        context: state.context,
        tokens: state.output,
        final_offset_utf16: input.final_offset_utf16,
        final_position_increment: input.final_position_increment,
        terminal,
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
        let (token, more) = if let Some(token) = self.input.next() {
            (Some(token), true)
        } else {
            (self.terminal.take().map(|token| *token), false)
        };
        if let Some(token) = token {
            self.total_units(&token, token.term_len())?;
            self.current = Some(token);
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
        if !super::numeral(&current.term(), &mut self.work)? {
            return Ok(true);
        }
        self.compose()
    }

    fn compose(&mut self) -> AnalysisResult<bool> {
        let original = self.current.as_ref().expect("numeric attributes").clone();
        let mut term = original.term().into_owned();
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
                self.saved = Some(current.clone());
                self.current = Some(original);
                self.changed = true;
                // The existing numeral prefix intentionally survives an aborted composition.
                return Ok(more);
            }
            let required = self
                .numeral
                .len()
                .checked_add(term.len())
                .ok_or_else(|| invalid("Nori number", "numeral size overflow"))?;
            check_limit("Nori numeric units", required, self.limits.max_output_utf16)?;
            self.numeral
                .try_reserve(term.len())
                .map_err(DictionaryError::from)?;
            for unit in &term {
                self.work.tick()?;
                self.numeral.push(*unit);
            }
            if !more {
                break false;
            }
            term.clear();
            term.extend_from_slice(&current.term());
            if !super::numeral(&term, &mut self.work)?
                && !super::punctuation(&term, &mut self.work)?
            {
                break true;
            }
        };
        if more {
            self.saved = self.current.clone();
        }
        let mut current = self.current.take().expect("composed attributes");
        current.cover(&first, &last, &self.context)?;
        let metadata = self.total_units(&current, 0)?;
        let maximum = self.limits.max_output_utf16 - metadata;
        let numeral = std::mem::take(&mut self.numeral);
        current.replace_term(
            super::normalize(&numeral, maximum, &mut self.work)?,
            &self.context,
        );
        self.current = Some(current);
        self.changed = true;
        Ok(true)
    }
}
