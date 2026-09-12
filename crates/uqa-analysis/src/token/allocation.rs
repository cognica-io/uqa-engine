//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Token buffers retain their terms and morphology until the result changes owner.

use std::ops::Range;

use uqa_core::memory::{Budgeted, BudgetedVec, MemoryBudget, MemoryReservation};

use super::{AnalysisToken, AnalyzedText, TokenBatch};
use crate::{AnalysisResult, FilteredText};

pub(crate) struct TokenBuffer {
    pub(super) tokens: BudgetedVec<AnalysisToken>,
    terminal: Option<Box<AnalysisToken>>,
    pub(super) memory: MemoryReservation,
}

impl TokenBuffer {
    pub fn new(budget: &MemoryBudget) -> Self {
        Self {
            tokens: BudgetedVec::new(budget),
            terminal: None,
            memory: budget.empty_reservation(),
        }
    }

    pub fn push(&mut self, token: Budgeted<AnalysisToken>) -> AnalysisResult<()> {
        self.tokens.reserve(1)?;
        let (token, memory) = token.into_parts();
        self.memory.absorb(memory);
        self.tokens.push(token)?;
        Ok(())
    }

    #[cfg(feature = "nori")]
    pub(super) fn set_terminal(&mut self, token: AnalysisToken) -> AnalysisResult<()> {
        self.memory.grow(std::mem::size_of::<AnalysisToken>())?;
        self.terminal = Some(Box::new(token));
        Ok(())
    }

    pub fn finish(
        self,
        input: &FilteredText<'_>,
        final_position_increment: u32,
        poll: &mut dyn FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<Budgeted<AnalyzedText>> {
        poll()?;
        #[cfg(feature = "nori")]
        let projection = input.projection_budgeted(self.memory.budget(), poll)?;
        let (tokens, mut memory) = self.tokens.into_parts();
        memory.absorb(self.memory);
        let output = Budgeted::new(
            AnalyzedText {
                batch: TokenBatch {
                    tokens,
                    final_position_increment,
                    terminal: self.terminal,
                },
                final_offsets: input.final_offsets(),
                #[cfg(feature = "nori")]
                projection,
            },
            memory,
        );
        output.batch.validate_positions_with_control(poll)?;
        poll()?;
        Ok(output)
    }
}

impl AnalysisToken {
    pub(crate) fn from_source_budgeted(
        input: &FilteredText<'_>,
        range: Range<usize>,
        budget: &MemoryBudget,
        poll: &mut dyn FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<Budgeted<Self>> {
        poll()?;
        let offsets = input.source_offsets(range.clone())?;
        let filtered_utf16 = Some(input.filtered_utf16(range.clone())?);
        let source = &input.as_str()[range];
        let verbatim = input.original().get(offsets.utf8.clone()) == Some(source);
        let (term, memory) = crate::allocation::copy_text(source, budget, poll)?.into_parts();
        Ok(Budgeted::new(
            Self {
                term: term.into(),
                offsets: Some(offsets),
                position_increment: 1,
                position_length: 1,
                keyword: false,
                filtered_utf16,
                #[cfg(feature = "nori")]
                korean_morphology: None,
                verbatim,
            },
            memory,
        ))
    }
}
