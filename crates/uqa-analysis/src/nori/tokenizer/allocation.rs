//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Token-owned strings and code units are reserved before materialization.

use uqa_core::memory::{Budgeted, BudgetedVec, MemoryBudget, MemoryError};

use crate::nori::error::check_limit;
use crate::AnalysisResult;

pub(super) fn encode(
    input: &str,
    limit: usize,
    budget: &MemoryBudget,
    poll: &mut dyn FnMut() -> AnalysisResult<()>,
) -> AnalysisResult<Budgeted<Vec<u16>>> {
    let length = utf16_len(input, limit, poll)?;
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

pub(super) fn utf16_len(
    input: &str,
    limit: usize,
    poll: &mut dyn FnMut() -> AnalysisResult<()>,
) -> AnalysisResult<usize> {
    let mut length = 0;
    for (index, unit) in input.chars().enumerate() {
        if index % 1024 == 0 {
            poll()?;
        }
        length += unit.len_utf16();
        check_limit("Nori input UTF-16 units", length, limit)?;
    }
    Ok(length)
}

pub(super) fn copy_units(
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

pub(super) fn copy_string(
    input: &str,
    budget: &MemoryBudget,
    poll: &mut dyn FnMut() -> AnalysisResult<()>,
) -> AnalysisResult<Budgeted<String>> {
    poll()?;
    let memory = budget.reserve(input.len())?;
    let mut output = String::new();
    output
        .try_reserve_exact(input.len())
        .map_err(MemoryError::from)?;
    for (index, character) in input.chars().enumerate() {
        if index % 1024 == 0 {
            poll()?;
        }
        output.push(character);
    }
    Ok(Budgeted::new(output, memory))
}
