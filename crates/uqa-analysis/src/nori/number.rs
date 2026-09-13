//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exact Korean numeral parsing and reference-compatible token composition.

use super::error::{check_limit, invalid};
use super::filters::Work;
use super::NoriLimits;
use crate::AnalysisResult;
use uqa_core::memory::{Budgeted, BudgetedVec, MemoryBudget};

mod decimal;
mod parse;
mod stream;

pub(super) use stream::filter;

struct Context<'a, 'b> {
    work: &'a mut Work<'b>,
    budget: &'a MemoryBudget,
    maximum: usize,
}

impl Context<'_, '_> {
    fn check_digits(&self, digits: usize) -> AnalysisResult<()> {
        check_limit("Nori numeric units", digits, self.maximum)?;
        Ok(())
    }
}

/// Normalize the reference numeric prefix with exact decimal arithmetic.
///
/// Malformed decimals or absent numeric prefixes retain the complete input. A successfully parsed prefix discards the remaining suffix, as Lucene does. Resource failures remain errors.
///
/// ```
/// use uqa_analysis::nori::normalize_number;
/// assert_eq!(normalize_number("３．２천")?, "3200");
/// assert_eq!(normalize_number("15,7")?, "157");
/// assert_eq!(normalize_number("12원")?, "12");
/// assert_eq!(normalize_number("1.2.3")?, "1.2.3");
/// # Ok::<(), uqa_analysis::AnalysisError>(())
/// ```
pub fn normalize_number(input: &str) -> AnalysisResult<String> {
    Ok(normalize_number_budgeted(
        input,
        NoriLimits::default(),
        &MemoryBudget::new(usize::MAX),
        &mut || Ok(()),
    )?
    .into_parts()
    .0)
}

/// Normalize a scalar numeral while retaining reservations for coefficients and both output encodings.
///
/// Input encoding, numeric coefficient buffers, and the returned text reserve bytes before allocation. A replacement coexists with its predecessor in the allowance. Count limits remain active; allocation or callback errors publish no partial result. Borrowed input and allocator bookkeeping are outside these reservations.
///
/// ```
/// use uqa_analysis::nori::{normalize_number_budgeted, NoriLimits};
/// use uqa_core::memory::MemoryBudget;
/// let budget = MemoryBudget::new(64 * 1024);
/// let output = normalize_number_budgeted(
///     "３．２천", NoriLimits::default(), &budget, &mut || Ok(()),
/// )?;
/// assert_eq!(output.as_str(), "3200");
/// assert_eq!(budget.used(), output.reserved_bytes());
/// drop(output);
/// assert_eq!(budget.used(), 0);
/// # Ok::<(), uqa_analysis::AnalysisError>(())
/// ```
pub fn normalize_number_budgeted(
    input: &str,
    limits: NoriLimits,
    budget: &MemoryBudget,
    poll: &mut impl FnMut() -> AnalysisResult<()>,
) -> AnalysisResult<Budgeted<String>> {
    let units = super::tokenizer::allocation::encode(input, limits.max_input_utf16, budget, poll)?;
    let normalized = normalize_number_utf16_budgeted(&units, limits, budget, poll)?;
    drop(units);
    let (term, memory) = crate::TokenTerm::from_utf16_budgeted(normalized, poll)?.into_parts();
    let text = term
        .into_string()
        .map_err(|_| invalid("Nori number", "invalid scalar result"))?;
    Ok(Budgeted::new(text, memory))
}

/// Apply numeric-prefix normalization to raw UTF-16 with explicit limits and cancellation.
///
/// Malformed decimals or absent numeric prefixes retain all original units, including unpaired surrogates. The output-unit limit also bounds intermediate numeric coefficients and formatting.
pub fn normalize_number_utf16(
    input: &[u16],
    limits: NoriLimits,
    poll: &mut impl FnMut() -> AnalysisResult<()>,
) -> AnalysisResult<Vec<u16>> {
    Ok(
        normalize_number_utf16_budgeted(input, limits, &MemoryBudget::new(usize::MAX), poll)?
            .into_parts()
            .0,
    )
}

/// Normalize lossless UTF-16 with one allowance for numeric coefficients and the returned buffer.
///
/// Malformed decimal input and absent numeric prefixes are copied exactly, including isolated surrogates. The caller's borrowed input has separate ownership. Byte-limit and callback errors propagate without being converted into a successful unchanged result.
pub fn normalize_number_utf16_budgeted(
    input: &[u16],
    limits: NoriLimits,
    budget: &MemoryBudget,
    poll: &mut impl FnMut() -> AnalysisResult<()>,
) -> AnalysisResult<Budgeted<Vec<u16>>> {
    let mut work = Work::new(poll)?;
    check_limit(
        "Nori input UTF-16 units",
        input.len(),
        limits.max_input_utf16,
    )?;
    let result = normalize_budgeted(input, limits.max_output_utf16, budget, &mut work)?;
    work.finish()?;
    Ok(result)
}

fn normalize_budgeted(
    input: &[u16],
    maximum: usize,
    budget: &MemoryBudget,
    work: &mut Work<'_>,
) -> AnalysisResult<Budgeted<Vec<u16>>> {
    let mut context = Context {
        work,
        budget,
        maximum,
    };
    context.check_digits(input.len())?;
    if let Some(decimal) = parse::parse(input, &mut context)? {
        return decimal.format(&mut context);
    }
    let mut original = BudgetedVec::new(budget);
    original.reserve(input.len())?;
    for unit in input {
        context.work.tick()?;
        original.push(*unit)?;
    }
    let (original, memory) = original.into_parts();
    Ok(Budgeted::new(original, memory))
}

fn digit(unit: u16) -> Option<u8> {
    Some(match unit {
        0x0030..=0x0039 => (unit - 0x0030) as u8,
        0xff10..=0xff19 => (unit - 0xff10) as u8,
        0xc601 => 0,
        0xc77c => 1,
        0xc774 => 2,
        0xc0bc => 3,
        0xc0ac => 4,
        0xc624 => 5,
        0xc721 => 6,
        0xce60 => 7,
        0xd314 => 8,
        0xad6c => 9,
        _ => return None,
    })
}

fn exponent(unit: u16) -> usize {
    match unit {
        0xc2ed => 1,
        0xbc31 => 2,
        0xcc9c => 3,
        0xb9cc => 4,
        0xc5b5 => 8,
        0xc870 => 12,
        0xacbd => 16,
        0xd574 => 20,
        _ => 0,
    }
}

fn numeral(input: impl Iterator<Item = u16>, work: &mut Work<'_>) -> AnalysisResult<bool> {
    for unit in input {
        work.tick()?;
        if digit(unit).is_none() && exponent(unit) == 0 {
            return Ok(false);
        }
    }
    Ok(true)
}

fn punctuation(input: impl Iterator<Item = u16>, work: &mut Work<'_>) -> AnalysisResult<bool> {
    for unit in input {
        work.tick()?;
        if !matches!(unit, 0x002e | 0xff0e | 0x002c | 0xff0c) {
            return Ok(false);
        }
    }
    Ok(true)
}
