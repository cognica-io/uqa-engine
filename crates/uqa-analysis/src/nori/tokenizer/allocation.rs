//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Token-owned strings and code units are reserved before materialization.

use uqa_core::memory::{Budgeted, MemoryBudget};

use crate::nori::error::check_limit;
use crate::AnalysisResult;

pub(super) use crate::allocation::{copy_text as copy_string, copy_units};

pub(in crate::nori) fn encode(
    input: &str,
    limit: usize,
    budget: &MemoryBudget,
    poll: &mut dyn FnMut() -> AnalysisResult<()>,
) -> AnalysisResult<Budgeted<Vec<u16>>> {
    crate::morphology::input::encode(input, budget, poll, |length| {
        check_limit("Nori input UTF-16 units", length, limit).map_err(Into::into)
    })
}

pub(super) fn utf16_len(
    input: &str,
    limit: usize,
    poll: &mut dyn FnMut() -> AnalysisResult<()>,
) -> AnalysisResult<usize> {
    crate::morphology::input::utf16_len(input, poll, |length| {
        check_limit("Nori input UTF-16 units", length, limit).map_err(Into::into)
    })
}
