//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Reserved UTF-16 input conversion with language-owned limit diagnostics.

use uqa_core::memory::{Budgeted, BudgetedVec, MemoryBudget};

use crate::{
    work::progress::{Callback, Progress},
    AnalysisError, AnalysisResult,
};

pub(crate) fn encode(
    input: &str,
    budget: &MemoryBudget,
    poll: &mut dyn FnMut() -> AnalysisResult<()>,
    check: impl FnMut(usize) -> AnalysisResult<()>,
) -> AnalysisResult<Budgeted<Vec<u16>>> {
    crate::work::inline(encode_with(input, budget, &mut Callback(poll), check))
}

pub(crate) async fn encode_with(
    input: &str,
    budget: &MemoryBudget,
    progress: &mut impl Progress<AnalysisError>,
    check: impl FnMut(usize) -> AnalysisResult<()>,
) -> AnalysisResult<Budgeted<Vec<u16>>> {
    let length = utf16_len_with(input, progress, check).await?;
    progress.checkpoint().await?;
    let mut output = BudgetedVec::new(budget);
    output.reserve(length)?;
    for (index, unit) in input.encode_utf16().enumerate() {
        if index % 1024 == 0 {
            progress.checkpoint().await?;
        }
        output.push(unit)?;
    }
    let (output, memory) = output.into_parts();
    Ok(Budgeted::new(output, memory))
}

pub(crate) fn utf16_len(
    input: &str,
    poll: &mut dyn FnMut() -> AnalysisResult<()>,
    check: impl FnMut(usize) -> AnalysisResult<()>,
) -> AnalysisResult<usize> {
    crate::work::inline(utf16_len_with(input, &mut Callback(poll), check))
}

pub(crate) async fn utf16_len_with(
    input: &str,
    progress: &mut impl Progress<AnalysisError>,
    mut check: impl FnMut(usize) -> AnalysisResult<()>,
) -> AnalysisResult<usize> {
    let mut length = 0;
    for (index, unit) in input.chars().enumerate() {
        if index % 1024 == 0 {
            progress.checkpoint().await?;
        }
        length += unit.len_utf16();
        check(length)?;
    }
    Ok(length)
}
