//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Transfer native morphology leases while replacing UTF-16 term and token buffers.

use uqa_core::memory::Budgeted;

use crate::nori::NoriOutput;
use crate::token::TokenBatch;
use crate::{AnalysisResult, AnalyzedText, FilteredText};

impl AnalyzedText {
    pub(crate) fn from_nori_budgeted(
        output: Budgeted<NoriOutput>,
        input: &FilteredText<'_>,
        poll: &mut dyn FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<Budgeted<Self>> {
        let (output, memory) = output.into_parts();
        super::super::native::output_budgeted(
            TokenBatch {
                tokens: output.tokens,
                final_position_increment: output.final_position_increment,
                terminal: output.terminal,
            },
            output.final_offset_utf16,
            memory,
            input,
            poll,
        )
    }
}
