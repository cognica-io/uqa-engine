//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stemmer scratch and result buffers share the caller's allocation allowance.

use uqa_core::memory::{Budgeted, BudgetedString, BudgetedVec, MemoryBudget, MemoryError};

use super::{algorithm, word::Word, Character};
use crate::{AnalysisResult, TokenTerm};

pub fn stem(word: &str) -> String {
    stem_budgeted(word, &MemoryBudget::new(usize::MAX), || Ok(()))
        .expect("unbounded Porter stemming")
        .into_parts()
        .0
}

pub(crate) fn stem_utf16(word: &[u16]) -> Vec<u16> {
    stem_utf16_budgeted(word, &MemoryBudget::new(usize::MAX), &mut || Ok(()))
        .expect("unbounded lossless Porter stemming")
        .into_parts()
        .0
}

/// Stem scalar text with reserved scratch/output buffers and cancellation checks.
///
/// Input is borrowed. The result retains its string reservation; scratch is released before return. Character loading, prefix scans, suffix stages, and encoding poll for cancellation. Consonant state for repeated `y` is computed without recursion.
///
/// ```
/// use uqa_analysis::porter::stem_budgeted;
/// use uqa_core::memory::MemoryBudget;
/// let budget = MemoryBudget::new(4096);
/// let result = stem_budgeted("relational", &budget, || Ok(()))?;
/// assert_eq!(&**result, "relat");
/// assert_eq!(budget.used(), result.reserved_bytes());
/// assert!(budget.peak() > budget.used());
/// drop(result);
/// assert_eq!(budget.used(), 0);
/// # Ok::<(), uqa_analysis::AnalysisError>(())
/// ```
pub fn stem_budgeted(
    input: &str,
    budget: &MemoryBudget,
    mut poll: impl FnMut() -> AnalysisResult<()>,
) -> AnalysisResult<Budgeted<String>> {
    let mut word = Word::new(input.chars().map(Character::from), budget, &mut poll)?;
    algorithm::apply(&mut word)?;
    let mut length = 0usize;
    for index in 0..word.len() {
        if index % 1024 == 0 {
            word.check()?;
        }
        let character = char::from_u32(word[index].0).expect("scalar input and ASCII suffixes");
        length = length
            .checked_add(character.len_utf8())
            .ok_or(MemoryError::SizeOverflow)?;
    }
    let mut output = BudgetedString::new(budget);
    output.reserve(length)?;
    for index in 0..word.len() {
        if index % 1024 == 0 {
            word.check()?;
        }
        output.push(char::from_u32(word[index].0).expect("scalar input and ASCII suffixes"))?;
    }
    word.check()?;
    drop(word);
    let (output, memory) = output.into_parts();
    Ok(Budgeted::new(output, memory))
}

/// Stem a complete lossless term, including isolated surrogate elements.
pub fn stem_term_budgeted(
    input: &TokenTerm,
    budget: &MemoryBudget,
    mut poll: impl FnMut() -> AnalysisResult<()>,
) -> AnalysisResult<Budgeted<TokenTerm>> {
    if let Some(input) = input.as_str() {
        let (term, memory) = stem_budgeted(input, budget, poll)?.into_parts();
        Ok(Budgeted::new(term.into(), memory))
    } else {
        let output = stem_utf16_budgeted(&input.utf16(), budget, &mut poll)?;
        TokenTerm::from_utf16_budgeted(output, poll)
    }
}

pub(super) fn stem_utf16_budgeted(
    input: &[u16],
    budget: &MemoryBudget,
    poll: &mut dyn FnMut() -> AnalysisResult<()>,
) -> AnalysisResult<Budgeted<Vec<u16>>> {
    let characters = char::decode_utf16(input.iter().copied()).map(|value| {
        value.map_or_else(
            |error| Character(u32::from(error.unpaired_surrogate())),
            Character::from,
        )
    });
    let mut word = Word::new(characters, budget, poll)?;
    algorithm::apply(&mut word)?;
    let mut length = 0usize;
    for index in 0..word.len() {
        if index % 1024 == 0 {
            word.check()?;
        }
        let units = char::from_u32(word[index].0).map_or(1, char::len_utf16);
        length = length.checked_add(units).ok_or(MemoryError::SizeOverflow)?;
    }
    let mut output = BudgetedVec::new(budget);
    output.reserve(length)?;
    for index in 0..word.len() {
        if index % 1024 == 0 {
            word.check()?;
        }
        if let Some(character) = char::from_u32(word[index].0) {
            for unit in character.encode_utf16(&mut [0; 2]) {
                output.push(*unit)?;
            }
        } else {
            output.push(u16::try_from(word[index].0).expect("isolated surrogate"))?;
        }
    }
    word.check()?;
    drop(word);
    let (output, memory) = output.into_parts();
    Ok(Budgeted::new(output, memory))
}
