//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Reserved scalar and UTF-16 copies shared by source retention and token attributes.

use uqa_core::memory::{Budgeted, BudgetedString, BudgetedVec, MemoryBudget};

use crate::AnalysisResult;

pub(crate) fn copy_text(
    input: &str,
    budget: &MemoryBudget,
    poll: &mut dyn FnMut() -> AnalysisResult<()>,
) -> AnalysisResult<Budgeted<String>> {
    poll()?;
    let mut output = BudgetedString::new(budget);
    output.reserve(input.len())?;
    for (index, character) in input.chars().enumerate() {
        if index % 1024 == 0 {
            poll()?;
        }
        output.push(character)?;
    }
    let (output, memory) = output.into_parts();
    Ok(Budgeted::new(output, memory))
}

pub(crate) fn copy_units(
    input: &[u16],
    budget: &MemoryBudget,
    poll: &mut dyn FnMut() -> AnalysisResult<()>,
) -> AnalysisResult<Budgeted<Vec<u16>>> {
    poll()?;
    let mut output = BudgetedVec::new(budget);
    output.reserve(input.len())?;
    for (index, unit) in input.iter().enumerate() {
        if index % 1024 == 0 {
            poll()?;
        }
        output.push(*unit)?;
    }
    let (output, memory) = output.into_parts();
    Ok(Budgeted::new(output, memory))
}
