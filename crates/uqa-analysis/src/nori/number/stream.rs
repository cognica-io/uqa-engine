//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Number composition retains Lucene's shared lookahead and terminal attributes.

use crate::nori::error::{check_limit, invalid};
use crate::nori::filters::{token_units, Work};
use crate::nori::{DictionaryError, NoriLimits, NoriOutput, NoriToken};
use crate::{AnalysisError, AnalysisResult};

struct State<'a> {
    input: std::vec::IntoIter<NoriToken>,
    terminal: Option<Box<NoriToken>>,
    current: Option<NoriToken>,
    changed: bool,
    saved: Option<NoriToken>,
    numeral: Vec<u16>,
    fall_through: u32,
    exhausted: bool,
    output: Vec<NoriToken>,
    output_units: usize,
    limits: NoriLimits,
    work: Work<'a>,
}

pub(in crate::nori) fn filter(
    input: NoriOutput,
    limits: NoriLimits,
    poll: &mut impl FnMut() -> AnalysisResult<()>,
) -> AnalysisResult<NoriOutput> {
    let work = Work::new(poll)?;
    check_limit("Nori output tokens", input.tokens.len(), limits.max_tokens)?;
    check_limit(
        "Nori input UTF-16 units",
        input.final_offset_utf16,
        limits.max_input_utf16,
    )?;
    let mut state = State {
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
        state.output_units = state.total_units(&token, token.term_utf16.len())?;
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
        state.total_units(token, token.term_utf16.len())?;
    }
    state.work.finish()?;
    Ok(NoriOutput {
        tokens: state.output,
        final_offset_utf16: input.final_offset_utf16,
        final_position_increment: input.final_position_increment,
        terminal,
    })
}

impl State<'_> {
    fn total_units(&mut self, token: &NoriToken, term_units: usize) -> AnalysisResult<usize> {
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
            self.total_units(&token, token.term_utf16.len())?;
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
        if current.keyword {
            return Ok(true);
        }
        if self.fall_through > 0 {
            self.fall_through -= 1;
            return Ok(true);
        }
        if current.position_increment == 0 {
            self.fall_through = current
                .position_length
                .checked_sub(1)
                .ok_or(AnalysisError::InvalidTokenPosition)?;
            return Ok(true);
        }
        if !super::numeral(&current.term_utf16, &mut self.work)? {
            return Ok(true);
        }
        self.compose()
    }

    fn compose(&mut self) -> AnalysisResult<bool> {
        let original = self.current.as_ref().expect("numeric attributes").clone();
        let mut term = original.term_utf16.clone();
        let start = original.start_utf16;
        let mut end;
        let more = loop {
            self.work.tick()?;
            end = self.current.as_ref().expect("numeric attributes").end_utf16;
            let more = self.read()?;
            if !more {
                self.exhausted = true;
            }
            let current = self
                .current
                .as_ref()
                .expect("last successful or explicit terminal attributes");
            if current.position_increment == 0 {
                self.fall_through = current
                    .position_length
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
            term.clone_from(&current.term_utf16);
            if !super::numeral(&term, &mut self.work)?
                && !super::punctuation(&term, &mut self.work)?
            {
                break true;
            }
        };
        if more {
            self.saved = self.current.clone();
        }
        if start > end {
            return Err(AnalysisError::InvalidTextSpan { start, end });
        }
        let mut current = self.current.take().expect("composed attributes");
        let metadata = self.total_units(&current, 0)?;
        let maximum = self.limits.max_output_utf16 - metadata;
        let numeral = std::mem::take(&mut self.numeral);
        current.term_utf16 = super::normalize(&numeral, maximum, &mut self.work)?;
        current.start_utf16 = start;
        current.end_utf16 = end;
        self.current = Some(current);
        self.changed = true;
        Ok(true)
    }
}
