//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retained query terms release unused token morphology and preserve scalar/raw identity.

use crate::{AnalysisError, AnalysisResult, AnalysisToken, AnalyzedText, TokenTerm};
use uqa_core::memory::{Budgeted, BudgetedVec, MemoryBudget};
use uqa_core::ordering::sort_by_with_control as sort_by;

pub(super) struct Terms {
    values: BudgetedVec<Budgeted<TokenTerm>>,
}

impl Terms {
    pub fn new(budget: &MemoryBudget) -> Self {
        Self {
            values: BudgetedVec::new(budget),
        }
    }
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
    pub fn push(&mut self, term: Budgeted<TokenTerm>) -> AnalysisResult<()> {
        self.values.push(term)?;
        Ok(())
    }

    pub fn append(
        &mut self,
        input: Budgeted<AnalyzedText>,
        scalar: bool,
        poll: &mut dyn FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<()> {
        let mut input = AnalyzedText::into_token_input(input);
        loop {
            let (next, remaining) = input.next(poll)?;
            input = remaining;
            let Some(token) = next else {
                break;
            };
            if scalar {
                require_scalar(token.term(), poll)?;
            }
            self.values.push(AnalysisToken::into_term_budgeted(token))?;
        }
        Ok(())
    }

    pub fn sort_unique(
        &mut self,
        poll: &mut dyn FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<()> {
        sort_by(&mut self.values, poll, |left, right, poll| {
            left.cmp_with_control(right, poll)
        })?;
        let mut retained = 0;
        for index in 0..self.values.len() {
            poll()?;
            if retained == 0
                || !self.values[index].eq_with_control(&self.values[retained - 1], poll)?
            {
                self.values.swap(retained, index);
                retained += 1;
            }
        }
        while self.values.len() > retained {
            poll()?;
            self.values.pop();
        }
        Ok(())
    }

    pub fn contains(
        &self,
        term: &TokenTerm,
        poll: &mut dyn FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<bool> {
        let (mut start, mut end) = (0, self.values.len());
        while start < end {
            poll()?;
            let middle = start + (end - start) / 2;
            match self.values[middle].cmp_with_control(term, poll)? {
                std::cmp::Ordering::Less => start = middle + 1,
                std::cmp::Ordering::Greater => end = middle,
                std::cmp::Ordering::Equal => return Ok(true),
            }
        }
        Ok(false)
    }
}

pub(super) fn require_scalar(
    term: &TokenTerm,
    poll: &mut dyn FnMut() -> AnalysisResult<()>,
) -> AnalysisResult<()> {
    poll()?;
    if term.as_str().is_some() {
        return Ok(());
    }
    for (index, character) in term.characters().enumerate() {
        if index % 1024 == 0 {
            poll()?;
        }
        if let Err(unit) = character {
            return Err(AnalysisError::UnpairedTokenSurrogate { unit });
        }
    }
    unreachable!("raw term contains an isolated surrogate")
}
