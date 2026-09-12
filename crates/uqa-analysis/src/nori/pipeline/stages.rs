//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Frozen Korean filters execute over the common stream and its retained source projection.

use std::sync::Arc;

use super::super::{
    filters::{stream::FilterStream, CompiledFilter},
    KoreanFilter, NoriLimits, NoriResources, ResolvedDictionary,
};
use crate::{
    source::SourceProjection, token::TokenBatch, AnalysisResult, AnalyzedText, TokenFilter,
};

#[derive(Debug)]
pub(crate) struct PreparedNoriFilter {
    filter: CompiledFilter,
    profile: Option<Arc<ResolvedDictionary>>,
}

impl PreparedNoriFilter {
    pub fn new(filter: &KoreanFilter, profile: Option<Arc<ResolvedDictionary>>) -> Self {
        Self {
            filter: filter.compile(),
            profile,
        }
    }

    pub fn resolve(filter: &TokenFilter, resources: &NoriResources) -> AnalysisResult<Self> {
        let profile = if let TokenFilter::UnicodeSimpleLowercase(config) = filter {
            Some(super::load_profile(&config.unicode_profile, resources)?)
        } else {
            None
        };
        Ok(Self::new(
            &super::korean_filter(filter).expect("Korean filter variant"),
            profile,
        ))
    }

    pub fn filter_analyzed(&self, mut input: AnalyzedText) -> AnalysisResult<AnalyzedText> {
        input.batch = self.filter_batch(input.batch, input.projection.clone())?;
        Ok(input)
    }

    pub fn filter_batch(
        &self,
        input: TokenBatch,
        projection: Arc<SourceProjection>,
    ) -> AnalysisResult<TokenBatch> {
        let stream = FilterStream {
            tokens: input.tokens,
            terminal: input.terminal,
            final_position_increment: input.final_position_increment,
            final_offset_utf16: projection.filtered_len(),
            context: projection,
        };
        let stream = self.filter.apply_stream(
            stream,
            self.profile
                .as_ref()
                .map(|profile| profile.model().as_ref()),
            NoriLimits::default(),
            &mut || Ok(()),
        )?;
        let batch = TokenBatch {
            tokens: stream.tokens,
            terminal: stream.terminal,
            final_position_increment: stream.final_position_increment,
        };
        batch.validate_positions()?;
        Ok(batch)
    }
}
