//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Shared decimal-prefix traversal and number composition with language-owned grammar and limits.

use super::filter::{ComposingToken, Work};
use crate::{AnalysisError, AnalysisResult};
use uqa_core::memory::{Budgeted, BudgetedVec, MemoryBudget};

mod parse;
mod stream;
pub(crate) use parse::parse;
pub(crate) use stream::filter;

pub(crate) trait Symbols {
    fn digit(unit: u16) -> Option<u8>;
    fn exponent(unit: u16) -> usize;
    fn large_power(power: usize) -> bool;
    fn decimal_point(unit: u16) -> bool;
    fn separator(unit: u16) -> bool;
}

pub(crate) enum Resource {
    InputUnits,
    OutputUnits,
    NumericUnits,
    Tokens,
}

pub(crate) trait Policy<T: ComposingToken> {
    type Symbols: Symbols;
    fn maximum_output(&self) -> usize;
    fn check(&self, resource: Resource, required: usize) -> AnalysisResult<()>;
    fn invalid(&self, reason: &'static str) -> AnalysisError;
    fn token_units(
        &self,
        token: &T,
        term_units: usize,
        work: &mut Work<'_>,
    ) -> AnalysisResult<usize>;
    fn normalize(
        &self,
        input: &[u16],
        maximum: usize,
        budget: &MemoryBudget,
        work: &mut Work<'_>,
    ) -> AnalysisResult<Budgeted<Vec<u16>>>;
}

fn numeral<S: Symbols>(
    input: impl Iterator<Item = u16>,
    work: &mut Work<'_>,
) -> AnalysisResult<bool> {
    for unit in input {
        work.tick()?;
        if S::digit(unit).is_none() && S::exponent(unit) == 0 {
            return Ok(false);
        }
    }
    Ok(true)
}
fn punctuation<S: Symbols>(
    input: impl Iterator<Item = u16>,
    work: &mut Work<'_>,
) -> AnalysisResult<bool> {
    for unit in input {
        work.tick()?;
        if !S::decimal_point(unit) && !S::separator(unit) {
            return Ok(false);
        }
    }
    Ok(true)
}

pub(crate) fn normalize<S: Symbols>(
    input: &[u16],
    context: &mut impl super::decimal::Context,
) -> AnalysisResult<Budgeted<Vec<u16>>> {
    context.check_digits(input.len())?;
    if let Some(decimal) = parse::<S>(input, context)? {
        return decimal.format(context);
    }
    let mut original = BudgetedVec::new(context.budget());
    original.reserve(input.len())?;
    for unit in input {
        context.tick()?;
        original.push(*unit)?;
    }
    let (original, memory) = original.into_parts();
    Ok(Budgeted::new(original, memory))
}
