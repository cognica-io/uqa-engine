//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! One reserved scalar conversion path serves explicit plans and native language normalizers.

use crate::{AnalysisError, AnalysisResult, CharFilter, FilteredText, TokenTerm};
use uqa_core::memory::{Budgeted, MemoryBudget};

pub(crate) trait Policy {
    fn input(&self, length: usize) -> AnalysisResult<()>;
    fn output(&self, length: usize) -> AnalysisResult<()>;
    fn lowercase(
        &self,
        _units: &mut [u16],
        _poll: &mut dyn FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<()> {
        Ok(())
    }
    fn invalid_scalar(&self) -> AnalysisError;
}

pub(crate) fn run(
    input: &str,
    width: bool,
    policy: &impl Policy,
    budget: &MemoryBudget,
    poll: &mut dyn FnMut() -> AnalysisResult<()>,
) -> AnalysisResult<Budgeted<String>> {
    poll()?;
    crate::allocation::input::utf16_len(input, poll, |length| policy.input(length))?;
    let filtered = if width {
        CharFilter::CJKWidth.filter_with_offsets_budgeted(input, budget, poll)?
    } else {
        FilteredText::new(input)
    };
    let units = crate::allocation::input::encode(filtered.as_str(), budget, poll, |length| {
        policy.output(length)
    })?;
    let (mut units, memory) = units.into_parts();
    policy.lowercase(&mut units, poll)?;
    drop(filtered);
    let (term, memory) =
        TokenTerm::from_utf16_budgeted(Budgeted::new(units, memory), &mut *poll)?.into_parts();
    let result = term.into_string().map_err(|_| policy.invalid_scalar())?;
    poll()?;
    Ok(Budgeted::new(result, memory))
}
