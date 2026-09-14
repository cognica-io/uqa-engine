//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Japanese reading replacement keeps absent/empty readings distinct and borrows the input twice.

mod romaji;

use super::stream::JapaneseToken;
use crate::kuromoji::error::check_limit;
use crate::morphology::filter::Work;
use crate::AnalysisResult;
use uqa_core::memory::{Budgeted, BudgetedVec, MemoryBudget, MemoryError};

pub(super) fn replacement<T: JapaneseToken>(
    token: &T,
    use_romaji: bool,
    limit: usize,
    budget: &MemoryBudget,
    work: &mut Work<'_>,
) -> AnalysisResult<Option<Budgeted<Vec<u16>>>> {
    let reading = token.reading()?;
    if reading.is_none() && !use_romaji {
        let mut found = false;
        for unit in token.term() {
            work.tick()?;
            if (0x3041..=0x3096).contains(&unit) {
                found = true;
                break;
            }
        }
        if !found {
            return Ok(None);
        }
    }
    let output = if let Some(reading) = reading {
        build(|| reading.encode_utf16(), use_romaji, limit, budget, work)?
    } else {
        build(
            || {
                token.term().map(|unit| {
                    if (0x3041..=0x3096).contains(&unit) {
                        unit + 0x60
                    } else {
                        unit
                    }
                })
            },
            use_romaji,
            limit,
            budget,
            work,
        )?
    };
    Ok(Some(output))
}

fn build<I: Iterator<Item = u16>>(
    input: impl Fn() -> I,
    use_romaji: bool,
    limit: usize,
    budget: &MemoryBudget,
    work: &mut Work<'_>,
) -> AnalysisResult<Budgeted<Vec<u16>>> {
    let mut length = 0usize;
    visit(input(), use_romaji, work, |_| {
        length = length.checked_add(1).ok_or(MemoryError::SizeOverflow)?;
        check_limit("Kuromoji reading UTF-16 units", length, limit)?;
        Ok(())
    })?;
    let mut output = BudgetedVec::new(budget);
    output.reserve(length)?;
    visit(input(), use_romaji, work, |unit| {
        output.push(unit)?;
        Ok(())
    })?;
    let (output, memory) = output.into_parts();
    Ok(Budgeted::new(output, memory))
}

fn visit(
    input: impl Iterator<Item = u16>,
    use_romaji: bool,
    work: &mut Work<'_>,
    mut emit: impl FnMut(u16) -> AnalysisResult<()>,
) -> AnalysisResult<()> {
    if use_romaji {
        romaji::visit(input, work, emit)
    } else {
        for unit in input {
            work.tick()?;
            emit(unit)?;
        }
        Ok(())
    }
}
