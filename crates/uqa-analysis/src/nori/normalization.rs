//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Korean profiles and diagnostics adapt shared scalar normalization without graph filtering.

use super::{error::check_limit, NoriDictionary, NoriLimits};
use crate::morphology::filter::Work;
use crate::{AnalysisError, AnalysisResult};
use uqa_core::memory::{Budgeted, MemoryBudget};

struct Policy<'a> {
    model: &'a NoriDictionary,
    limits: NoriLimits,
}
impl crate::normalization::text::Policy for Policy<'_> {
    fn input(&self, length: usize) -> AnalysisResult<()> {
        check_limit(
            "Nori input UTF-16 units",
            length,
            self.limits.max_input_utf16,
        )
        .map_err(Into::into)
    }
    fn output(&self, length: usize) -> AnalysisResult<()> {
        check_limit(
            "Nori output UTF-16 units",
            length,
            self.limits.max_output_utf16,
        )
        .map_err(Into::into)
    }
    fn lowercase(
        &self,
        units: &mut [u16],
        poll: &mut dyn FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<()> {
        super::filters::lowercase::apply(units, Some(self.model), &mut Work::new(poll)?)
    }
    fn invalid_scalar(&self) -> AnalysisError {
        super::error::invalid("Nori normalization", "invalid scalar result").into()
    }
}

pub(crate) fn normalize_budgeted(
    input: &str,
    width: bool,
    model: &NoriDictionary,
    limits: NoriLimits,
    budget: &MemoryBudget,
    poll: &mut dyn FnMut() -> AnalysisResult<()>,
) -> AnalysisResult<Budgeted<String>> {
    crate::normalization::text::run(input, width, &Policy { model, limits }, budget, poll)
}
