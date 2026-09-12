//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable standalone morphology and a separate simple-lowercase normalization path.

use std::sync::Arc;

use super::filters::{self, CompiledFilter};
use super::{
    KoreanFilter, KoreanTokenizer, NoriDictionary, NoriLimits, NoriOptions, NoriOutput,
    UserDictionary,
};
use crate::AnalysisResult;

#[derive(Debug, Clone)]
pub struct KoreanAnalyzer {
    model: Arc<NoriDictionary>,
    tokenizer: KoreanTokenizer,
    filters: Vec<CompiledFilter>,
}

impl KoreanAnalyzer {
    /// Tokenize, remove the default POS stops, convert readings, and apply pinned simple lowercase.
    pub fn new(
        model: Arc<NoriDictionary>,
        user: Option<Arc<UserDictionary>>,
        options: NoriOptions,
    ) -> AnalysisResult<Self> {
        Self::with_filters(
            model,
            user,
            options,
            &[
                KoreanFilter::PartOfSpeech { stop_tags: None },
                KoreanFilter::ReadingForm,
                KoreanFilter::SimpleLowercase,
            ],
        )
    }

    /// Compile an explicitly ordered chain. Normalization remains simple lowercase only.
    pub fn with_filters(
        model: Arc<NoriDictionary>,
        user: Option<Arc<UserDictionary>>,
        options: NoriOptions,
        filters: &[KoreanFilter],
    ) -> AnalysisResult<Self> {
        let tokenizer = KoreanTokenizer::new(model.clone(), user, options)?;
        let mut compiled = super::io::vector(filters.len())?;
        compiled.extend(filters.iter().map(KoreanFilter::compile));
        Ok(Self {
            model,
            tokenizer,
            filters: compiled,
        })
    }

    pub fn analyze(&self, input: &str) -> AnalysisResult<NoriOutput> {
        self.analyze_controlled(input, NoriLimits::default(), &mut || Ok(()))
    }

    pub fn analyze_controlled(
        &self,
        input: &str,
        limits: NoriLimits,
        poll: &mut impl FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<NoriOutput> {
        let mut output = self.tokenizer.tokenize_controlled(input, limits, poll)?;
        for filter in &self.filters {
            output = filter.apply(output, &self.model, limits, poll)?;
        }
        Ok(output)
    }

    /// The complete text receives simple lowercase; tokenization, POS stops, and readings do not run.
    pub fn normalize(&self, input: &str) -> AnalysisResult<String> {
        self.normalize_controlled(input, NoriLimits::default(), &mut || Ok(()))
    }

    pub fn normalize_controlled(
        &self,
        input: &str,
        limits: NoriLimits,
        poll: &mut impl FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<String> {
        let units = super::tokenizer::encode_input(input, limits, poll)?;
        let normalized = self.normalize_utf16(&units, limits, poll)?;
        String::from_utf16(&normalized).map_err(|_| {
            super::error::invalid("Nori normalization", "invalid scalar result").into()
        })
    }

    /// Preserve raw unpaired units when normalizing a UTF-16 term.
    pub fn normalize_utf16(
        &self,
        input: &[u16],
        limits: NoriLimits,
        poll: &mut impl FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<Vec<u16>> {
        filters::normalize(input, &self.model, limits, poll)
    }
}
