//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Japanese numeral rules use shared exact decimals and retained lookahead composition.

use super::error::{check_limit, invalid};
use super::KuromojiLimits;
use crate::morphology::filter::Work;
use crate::AnalysisResult;
use uqa_core::memory::{Budgeted, MemoryBudget};

mod policy;
pub(super) use policy::filter;

struct Context<'a, 'b> {
    work: &'a mut Work<'b>,
    budget: &'a MemoryBudget,
    maximum: usize,
}

impl crate::morphology::decimal::Context for Context<'_, '_> {
    fn tick(&mut self) -> AnalysisResult<()> {
        self.work.tick()
    }
    fn budget(&self) -> &MemoryBudget {
        self.budget
    }
    fn check_digits(&self, digits: usize) -> AnalysisResult<()> {
        check_limit("Kuromoji numeric units", digits, self.maximum)?;
        Ok(())
    }
    fn invalid(&self, reason: &'static str) -> crate::AnalysisError {
        invalid("Kuromoji number", reason).into()
    }
}

/// Normalize the reference Japanese numeric prefix using exact decimal arithmetic.
///
/// Malformed decimals or absent numeric prefixes retain the complete input. A parsed prefix discards its remaining suffix. The grammar accepts ASCII/fullwidth digits, 〇一二三四五六七八九, and powers 十百千万億兆京垓; it does not accept signs or formal numerals such as 壱.
///
/// ```
/// use uqa_analysis::kuromoji::normalize_number;
/// assert_eq!(normalize_number("３．２千")?, "3200");
/// assert_eq!(normalize_number("一億二千万")?, "120000000");
/// assert_eq!(normalize_number("12円")?, "12");
/// assert_eq!(normalize_number("1.2.3")?, "1.2.3");
/// # Ok::<(), uqa_analysis::AnalysisError>(())
/// ```
pub fn normalize_number(input: &str) -> AnalysisResult<String> {
    Ok(normalize_number_budgeted(
        input,
        KuromojiLimits::default(),
        &MemoryBudget::new(usize::MAX),
        &mut || Ok(()),
    )?
    .into_parts()
    .0)
}

/// Normalize a scalar numeral with one allowance for encoding, exact coefficients and returned text.
///
/// Count limits and cancellation remain active during encoding, parsing and formatting. Borrowed input is outside the returned reservation; byte-limit or callback errors publish no partial result.
///
/// ```
/// use uqa_analysis::kuromoji::{normalize_number_budgeted, KuromojiLimits};
/// use uqa_core::memory::MemoryBudget;
/// let budget = MemoryBudget::new(64 * 1024);
/// let output = normalize_number_budgeted(
///     "３．２千", KuromojiLimits::default(), &budget, &mut || Ok(()),
/// )?;
/// assert_eq!(output.as_str(), "3200");
/// assert_eq!(budget.used(), output.reserved_bytes());
/// drop(output);
/// assert_eq!(budget.used(), 0);
/// # Ok::<(), uqa_analysis::AnalysisError>(())
/// ```
pub fn normalize_number_budgeted(
    input: &str,
    limits: KuromojiLimits,
    budget: &MemoryBudget,
    poll: &mut impl FnMut() -> AnalysisResult<()>,
) -> AnalysisResult<Budgeted<String>> {
    let units = crate::morphology::input::encode(input, budget, poll, |required| {
        check_limit(
            "Kuromoji input UTF-16 units",
            required,
            limits.max_input_utf16,
        )?;
        Ok(())
    })?;
    let normalized = normalize_number_utf16_budgeted(&units, limits, budget, poll)?;
    drop(units);
    let (term, memory) = crate::TokenTerm::from_utf16_budgeted(normalized, poll)?.into_parts();
    let text = term
        .into_string()
        .map_err(|_| invalid("Kuromoji number", "invalid scalar result"))?;
    Ok(Budgeted::new(text, memory))
}

/// Normalize lossless UTF-16 with explicit count limits and cancellation.
///
/// Absent prefixes and malformed decimals retain all units, including unpaired surrogates. The output-unit limit also bounds numeric coefficients and formatting.
pub fn normalize_number_utf16(
    input: &[u16],
    limits: KuromojiLimits,
    poll: &mut impl FnMut() -> AnalysisResult<()>,
) -> AnalysisResult<Vec<u16>> {
    Ok(
        normalize_number_utf16_budgeted(input, limits, &MemoryBudget::new(usize::MAX), poll)?
            .into_parts()
            .0,
    )
}

/// Retain coefficient and output reservations while normalizing borrowed raw UTF-16.
///
/// Resource and callback errors propagate without becoming an unchanged successful result. The caller owns the borrowed input separately.
pub fn normalize_number_utf16_budgeted(
    input: &[u16],
    limits: KuromojiLimits,
    budget: &MemoryBudget,
    poll: &mut impl FnMut() -> AnalysisResult<()>,
) -> AnalysisResult<Budgeted<Vec<u16>>> {
    let mut work = Work::new(poll)?;
    check_limit(
        "Kuromoji input UTF-16 units",
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
    crate::morphology::number::normalize::<policy::Symbols>(
        input,
        &mut Context {
            work,
            budget,
            maximum,
        },
    )
}
