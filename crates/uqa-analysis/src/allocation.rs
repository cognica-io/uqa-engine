//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Reserved text copies shared by source retention and token emission.

use uqa_core::memory::{Budgeted, BudgetedString, MemoryBudget};

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
