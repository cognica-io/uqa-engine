//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Japanese profiles and limits adapt the shared normalization conversion owner.

use super::{error::check_limit, KuromojiDictionary, KuromojiLimits};
use crate::morphology::filter::Work;
use crate::{AnalysisError, AnalysisResult};
use uqa_core::memory::{Budgeted, MemoryBudget};

struct Policy<'a> {
    model: Option<&'a KuromojiDictionary>,
    limits: KuromojiLimits,
}
impl crate::normalization::text::Policy for Policy<'_> {
    fn input(&self, length: usize) -> AnalysisResult<()> {
        check_limit(
            "Kuromoji input UTF-16 units",
            length,
            self.limits.max_input_utf16,
        )
        .map_err(Into::into)
    }
    fn output(&self, length: usize) -> AnalysisResult<()> {
        check_limit(
            "Kuromoji normalization output UTF-16 units",
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
        if let Some(model) = self.model {
            super::filters::lowercase(units, model, &mut Work::new(poll)?)?;
        }
        Ok(())
    }
    fn invalid_scalar(&self) -> AnalysisError {
        super::error::invalid("Kuromoji normalization", "invalid scalar result").into()
    }
}

pub(crate) fn normalize_budgeted(
    input: &str,
    width: bool,
    model: Option<&KuromojiDictionary>,
    limits: KuromojiLimits,
    budget: &MemoryBudget,
    poll: &mut dyn FnMut() -> AnalysisResult<()>,
) -> AnalysisResult<Budgeted<String>> {
    crate::normalization::text::run(input, width, &Policy { model, limits }, budget, poll)
}
