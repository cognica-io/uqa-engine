//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Prepared regex automata with analysis-owned search and capture allocation lifetimes.

use std::ops::Range;

use regex_automata::nfa::thompson::NFA;
use uqa_core::memory::{BudgetedVec, MemoryBudget};

use crate::AnalysisResult;

mod control;
mod dfa;
mod nfa;
mod states;

// Match the validated regex implementation's default Thompson program limit.
const NFA_SIZE_LIMIT: usize = 10 << 20;
// Rust slices cannot reach this offset, so an owned usize slot can represent absence without private dependency layouts.
const ABSENT: usize = usize::MAX;

#[derive(Debug)]
pub(crate) struct CooperativeRegex {
    nfa: NFA,
    dfa: Option<dfa::RangeDFA>,
}

impl CooperativeRegex {
    pub(crate) fn compile(pattern: &str) -> Result<Self, regex::Error> {
        let nfa = NFA::compiler()
            .configure(
                NFA::config()
                    .nfa_size_limit(Some(NFA_SIZE_LIMIT))
                    .shrink(false),
            )
            .build(pattern)
            .map_err(|error| match error.size_limit() {
                Some(limit) => regex::Error::CompiledTooBig(limit),
                None => regex::Error::Syntax(error.to_string()),
            })?;
        Ok(Self {
            nfa,
            dfa: dfa::RangeDFA::compile(pattern),
        })
    }

    pub(crate) fn searcher<'a>(
        &'a self,
        budget: &MemoryBudget,
        captures: bool,
        poll: &mut dyn FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<Searcher<'a>> {
        poll()?;
        let slots = if captures {
            control::Control::new(poll).buffer(budget, self.nfa.group_info().slot_len(), ABSENT)?
        } else {
            BudgetedVec::new(budget)
        };
        Ok(Searcher {
            expression: self,
            captures,
            matched: false,
            slots: CaptureSlots { values: slots },
            cache: None,
        })
    }
}

pub(crate) struct CaptureSlots {
    values: BudgetedVec<usize>,
}

impl CaptureSlots {
    pub(crate) fn get(&self, group: usize) -> Option<(usize, usize)> {
        let index = group.checked_mul(2)?;
        let start = *self.values.get(index)?;
        let end = *self.values.get(index.checked_add(1)?)?;
        (start != ABSENT && end != ABSENT).then_some((start, end))
    }
}

pub(crate) struct Searcher<'a> {
    expression: &'a CooperativeRegex,
    captures: bool,
    matched: bool,
    slots: CaptureSlots,
    cache: Option<nfa::Cache>,
}

impl Searcher<'_> {
    pub(crate) fn find_at(
        &mut self,
        text: &str,
        mut start: usize,
        poll: &mut dyn FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<Option<Range<usize>>> {
        self.matched = false;
        poll()?;
        if start > text.len() {
            return Ok(None);
        }
        while !text.is_char_boundary(start) {
            start += 1;
        }
        let mut span = start..text.len();
        let mut anchored = false;
        if let Some(dfa) = &self.expression.dfa {
            match dfa.find_at(text, start, poll) {
                Ok(None) => return Ok(None),
                Ok(Some(range)) if !self.captures => return Ok(Some(range)),
                Ok(Some(range)) => {
                    span = range;
                    anchored = true;
                }
                Err(dfa::SearchError::Poll(error)) => return Err(error),
                Err(dfa::SearchError::Automaton) => {}
            }
        }
        let mut control = control::Control::new(poll);
        if self.slots.values.is_empty() {
            self.slots.values = control.buffer(self.slots.values.budget(), 2, ABSENT)?;
        }
        if self.cache.is_none() {
            self.cache = Some(nfa::Cache::new(
                &self.expression.nfa,
                self.slots.values.len(),
                self.slots.values.budget(),
                &mut control,
            )?);
        }
        let cache = self.cache.as_mut().expect("initialized regex cache");
        self.matched = cache.search(
            &self.expression.nfa,
            text,
            span,
            anchored,
            &mut self.slots.values,
            &mut control,
        )?;
        Ok(self.matched.then(|| {
            let (start, end) = self.slots.get(0).expect("NFA records the overall match");
            start..end
        }))
    }

    pub(crate) fn captures(&self) -> Option<&CaptureSlots> {
        (self.captures && self.matched).then_some(&self.slots)
    }
}

#[cfg(test)]
mod tests;
