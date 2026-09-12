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

mod decimal;
mod parse;
mod stream;

pub(super) use stream::filter;

struct Context<'a, 'b> {
    work: &'a mut Work<'b>,
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
    let limits = NoriLimits::default();
    let units = super::tokenizer::encode_input(input, limits, &mut || Ok(()))?;
    let normalized = normalize_number_utf16(&units, limits, &mut || Ok(()))?;
    String::from_utf16(&normalized)
        .map_err(|_| invalid("Nori number", "invalid scalar result").into())
}

/// Apply numeric-prefix normalization to raw UTF-16 with explicit limits and cancellation.
///
/// Malformed decimals or absent numeric prefixes retain all original units, including unpaired surrogates. The output-unit limit also bounds intermediate numeric coefficients and formatting.
pub fn normalize_number_utf16(
    input: &[u16],
    limits: NoriLimits,
    poll: &mut impl FnMut() -> AnalysisResult<()>,
) -> AnalysisResult<Vec<u16>> {
    let mut work = Work::new(poll)?;
    check_limit(
        "Nori input UTF-16 units",
        input.len(),
        limits.max_input_utf16,
    )?;
    let result = normalize(input, limits.max_output_utf16, &mut work)?;
    work.finish()?;
    Ok(result)
}

fn normalize(input: &[u16], maximum: usize, work: &mut Work<'_>) -> AnalysisResult<Vec<u16>> {
    let mut context = Context { work, maximum };
    context.check_digits(input.len())?;
    if let Some(decimal) = parse::parse(input, &mut context)? {
        return decimal.format(&mut context);
    }
    let mut original = super::io::vector(input.len())?;
    for unit in input {
        context.work.tick()?;
        original.push(*unit);
    }
    Ok(original)
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

fn numeral(input: &[u16], work: &mut Work<'_>) -> AnalysisResult<bool> {
    for unit in input {
        work.tick()?;
        if digit(*unit).is_none() && exponent(*unit) == 0 {
            return Ok(false);
        }
    }
    Ok(true)
}

fn punctuation(input: &[u16], work: &mut Work<'_>) -> AnalysisResult<bool> {
    for unit in input {
        work.tick()?;
        if !matches!(unit, 0x002e | 0xff0e | 0x002c | 0xff0c) {
            return Ok(false);
        }
    }
    Ok(true)
}
