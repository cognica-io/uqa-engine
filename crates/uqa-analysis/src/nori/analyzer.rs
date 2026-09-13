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
use uqa_core::memory::{Budgeted, MemoryBudget};

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

    /// Return generic tokens with lossless terms, Korean morphology, and original source offsets.
    pub fn analyze_tokens(&self, input: &str) -> AnalysisResult<crate::AnalyzedText> {
        self.analyze_mapped(&crate::FilteredText::new(input))
    }

    /// Analyze character-filter output and compose its exact source coordinates once.
    pub fn analyze_mapped(
        &self,
        input: &crate::FilteredText<'_>,
    ) -> AnalysisResult<crate::AnalyzedText> {
        Ok(self
            .analyze_mapped_budgeted(
                input,
                NoriLimits::default(),
                &MemoryBudget::new(usize::MAX),
                &mut || Ok(()),
            )?
            .into_parts()
            .0)
    }

    pub fn analyze_controlled(
        &self,
        input: &str,
        limits: NoriLimits,
        poll: &mut impl FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<NoriOutput> {
        Ok(self
            .analyze_budgeted(input, limits, &MemoryBudget::new(usize::MAX), poll)?
            .into_parts()
            .0)
    }

    /// Retain one allocation allowance from native tokenization through all configured Korean filters.
    pub fn analyze_budgeted(
        &self,
        input: &str,
        limits: NoriLimits,
        budget: &MemoryBudget,
        poll: &mut impl FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<Budgeted<NoriOutput>> {
        let mut output = self
            .tokenizer
            .tokenize_budgeted(input, limits, budget, poll)?;
        for filter in &self.filters {
            output = filter.apply_budgeted(output, &self.model, limits, poll)?;
        }
        Ok(output)
    }

    /// Analyze into reserved common tokens and preserve the source view after the call.
    pub fn analyze_tokens_budgeted(
        &self,
        input: &str,
        limits: NoriLimits,
        budget: &MemoryBudget,
        poll: &mut impl FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<Budgeted<crate::AnalyzedText>> {
        self.analyze_mapped_budgeted(&crate::FilteredText::new(input), limits, budget, poll)
    }

    /// Analyze mapped input without publishing newly prepared coordinate caches into the borrowed view.
    pub fn analyze_mapped_budgeted(
        &self,
        input: &crate::FilteredText<'_>,
        limits: NoriLimits,
        budget: &MemoryBudget,
        poll: &mut impl FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<Budgeted<crate::AnalyzedText>> {
        let input = input.clone();
        input.prepare_coordinates(budget, poll)?;
        let output = self.analyze_budgeted(input.as_str(), limits, budget, poll)?;
        crate::AnalyzedText::from_nori_budgeted(output, &input, poll)
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
        Ok(self
            .normalize_budgeted(input, limits, &MemoryBudget::new(usize::MAX), poll)?
            .into_parts()
            .0)
    }

    /// Normalize complete scalar input with reserved encoding and output buffers.
    pub fn normalize_budgeted(
        &self,
        input: &str,
        limits: NoriLimits,
        budget: &MemoryBudget,
        poll: &mut impl FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<Budgeted<String>> {
        filters::normalize_text_budgeted(input, &self.model, limits, budget, poll)
    }

    /// Preserve raw unpaired units when normalizing a UTF-16 term.
    pub fn normalize_utf16(
        &self,
        input: &[u16],
        limits: NoriLimits,
        poll: &mut impl FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<Vec<u16>> {
        Ok(self
            .normalize_utf16_budgeted(input, limits, &MemoryBudget::new(usize::MAX), poll)?
            .into_parts()
            .0)
    }

    /// Normalize raw UTF-16 while retaining output reservations and exact unpaired units.
    pub fn normalize_utf16_budgeted(
        &self,
        input: &[u16],
        limits: NoriLimits,
        budget: &MemoryBudget,
        poll: &mut impl FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<Budgeted<Vec<u16>>> {
        filters::normalize_budgeted(input, &self.model, limits, budget, poll)
    }
}
