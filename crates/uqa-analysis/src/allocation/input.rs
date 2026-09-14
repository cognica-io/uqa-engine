//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Reserved UTF-16 input conversion with language-owned limit diagnostics.

use uqa_core::memory::{Budgeted, BudgetedVec, MemoryBudget};

use crate::AnalysisResult;

pub(crate) fn encode(
    input: &str,
    budget: &MemoryBudget,
    poll: &mut dyn FnMut() -> AnalysisResult<()>,
    check: impl FnMut(usize) -> AnalysisResult<()>,
) -> AnalysisResult<Budgeted<Vec<u16>>> {
    let length = utf16_len(input, poll, check)?;
    poll()?;
    let mut output = BudgetedVec::new(budget);
    output.reserve(length)?;
    for (index, unit) in input.encode_utf16().enumerate() {
        if index % 1024 == 0 {
            poll()?;
        }
        output.push(unit)?;
    }
    let (output, memory) = output.into_parts();
    Ok(Budgeted::new(output, memory))
}

pub(crate) fn utf16_len(
    input: &str,
    poll: &mut dyn FnMut() -> AnalysisResult<()>,
    mut check: impl FnMut(usize) -> AnalysisResult<()>,
) -> AnalysisResult<usize> {
    let mut length = 0;
    for (index, unit) in input.chars().enumerate() {
        if index % 1024 == 0 {
            poll()?;
        }
        length += unit.len_utf16();
        check(length)?;
    }
    Ok(length)
}
