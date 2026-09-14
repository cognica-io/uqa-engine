//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Prepared Japanese filters retain their immutable lookup state and selected profile.

use std::sync::Arc;
use uqa_core::memory::{Budgeted, MemoryBudget};

use super::super::{filters::CompiledFilter, JapaneseFilter, KuromojiLimits, ResolvedDictionary};
use crate::{token::TokenBatch, AnalysisResult, AnalyzedText, FilteredText};

#[derive(Debug)]
pub(crate) struct PreparedKuromojiFilter {
    filter: Budgeted<CompiledFilter>,
    profile: Option<Arc<ResolvedDictionary>>,
}

impl PreparedKuromojiFilter {
    pub(super) fn new(
        filter: &JapaneseFilter,
        profile: Option<Arc<ResolvedDictionary>>,
    ) -> AnalysisResult<Self> {
        let filter = filter.compile(
            profile.as_ref().map(|profile| profile.model().as_ref()),
            KuromojiLimits::default(),
            &MemoryBudget::new(usize::MAX),
            &mut || Ok(()),
        )?;
        let profile = if filter.uses_model() { profile } else { None };
        Ok(Self { filter, profile })
    }

    pub(crate) fn filter_analyzed_budgeted(
        &self,
        input: Budgeted<AnalyzedText>,
        poll: &mut impl FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<Budgeted<AnalyzedText>> {
        self.filter.filter_analyzed_budgeted(
            input,
            self.profile
                .as_ref()
                .map(|profile| profile.model().as_ref()),
            KuromojiLimits::default(),
            poll,
        )
    }

    pub(crate) fn filter_batch(&self, batch: TokenBatch) -> AnalysisResult<TokenBatch> {
        let source = FilteredText::new("");
        let input = AnalyzedText {
            batch,
            projection: source.projection(),
            final_offsets: source.final_offsets(),
        };
        Ok(self
            .filter_analyzed_budgeted(input.into_unlimited()?, &mut || Ok(()))?
            .into_parts()
            .0
            .batch)
    }
}
