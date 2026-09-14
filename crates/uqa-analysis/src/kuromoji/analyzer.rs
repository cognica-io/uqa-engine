//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Standalone Japanese analysis applies width once and keeps normalization independent of filters.

use super::{
    error::check_limit, filters::CompiledFilter, JapaneseFilter, JapaneseTokenizer,
    KuromojiDictionary, KuromojiLimits, KuromojiMode, KuromojiOptions, UserDictionary,
};
use crate::morphology::filter::Work;
use crate::{AnalysisResult, AnalyzedText, CharFilter, FilteredText, TokenTerm};
use std::sync::Arc;
use uqa_core::memory::{Budgeted, BudgetedVec, MemoryBudget};

#[derive(Debug)]
pub struct JapaneseAnalyzer {
    model: Arc<KuromojiDictionary>,
    tokenizer: JapaneseTokenizer,
    filters: Budgeted<Vec<CompiledFilter>>,
}
impl JapaneseAnalyzer {
    /// Apply width and the selected tokenizer mode, then base form, POS/word stops, Katakana stem and simple lowercase.
    ///
    /// ```
    /// use uqa_analysis::kuromoji::{JapaneseAnalyzer, KuromojiMode, KuromojiResources};
    /// let dictionary = KuromojiResources::default().load_default()?;
    /// let analyzer = JapaneseAnalyzer::new(dictionary.model().clone(), None, KuromojiMode::Search)?;
    /// let output = analyzer.analyze("ＵＱＡで走りました")?;
    /// let terms: Vec<_> = output.tokens().iter().map(|token| token.term().as_str().unwrap()).collect();
    /// assert_eq!(terms, ["uqa", "走る"]);
    /// assert_eq!(analyzer.normalize("ＵＱＡで走りました")?, "uqaで走りました");
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn new(
        model: Arc<KuromojiDictionary>,
        user: Option<Arc<UserDictionary>>,
        mode: KuromojiMode,
    ) -> AnalysisResult<Self> {
        Self::with_filters(
            model,
            user,
            KuromojiOptions {
                mode,
                ..KuromojiOptions::default()
            },
            &[
                JapaneseFilter::BaseForm,
                JapaneseFilter::PartOfSpeech { stop_tags: None },
                JapaneseFilter::Stop {
                    words: None,
                    ignore_case: true,
                },
                JapaneseFilter::KatakanaStem { minimum_length: 4 },
                JapaneseFilter::SimpleLowercase,
            ],
        )
    }

    /// Compile an explicit native chain; normalization remains width plus simple lowercase.
    pub fn with_filters(
        model: Arc<KuromojiDictionary>,
        user: Option<Arc<UserDictionary>>,
        options: KuromojiOptions,
        filters: &[JapaneseFilter],
    ) -> AnalysisResult<Self> {
        Self::with_filters_budgeted(
            model,
            user,
            options,
            filters,
            KuromojiLimits::default(),
            &MemoryBudget::new(usize::MAX),
            &mut || Ok(()),
        )
    }

    /// Retain compiled lookup buffers under the preparation allowance; immutable models keep their owners.
    pub fn with_filters_budgeted(
        model: Arc<KuromojiDictionary>,
        user: Option<Arc<UserDictionary>>,
        options: KuromojiOptions,
        filters: &[JapaneseFilter],
        limits: KuromojiLimits,
        budget: &MemoryBudget,
        poll: &mut impl FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<Self> {
        poll()?;
        check_limit(
            "Kuromoji filter stages",
            filters.len(),
            limits.max_filter_entries,
        )?;
        let tokenizer = JapaneseTokenizer::new(model.clone(), user, options)?;
        let mut output = BudgetedVec::new(budget);
        output.reserve(filters.len())?;
        let mut memory = budget.empty_reservation();
        for filter in filters {
            let (filter, allocation) = filter.compile(&model, limits, budget, poll)?.into_parts();
            output.push(filter)?;
            memory.absorb(allocation);
        }
        let (filters, allocation) = output.into_parts();
        memory.absorb(allocation);
        poll()?;
        Ok(Self {
            model,
            tokenizer,
            filters: Budgeted::new(filters, memory),
        })
    }

    /// Return lossless common tokens, Japanese attributes and corrected original source coordinates.
    pub fn analyze(&self, input: &str) -> AnalysisResult<AnalyzedText> {
        self.analyze_mapped(&FilteredText::new(input))
    }
    pub fn analyze_mapped(&self, input: &FilteredText<'_>) -> AnalysisResult<AnalyzedText> {
        Ok(self
            .analyze_mapped_budgeted(
                input,
                KuromojiLimits::default(),
                &MemoryBudget::new(usize::MAX),
                &mut || Ok(()),
            )?
            .into_parts()
            .0)
    }
    pub fn analyze_budgeted(
        &self,
        input: &str,
        limits: KuromojiLimits,
        budget: &MemoryBudget,
        poll: &mut impl FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<Budgeted<AnalyzedText>> {
        self.analyze_mapped_budgeted(&FilteredText::new(input), limits, budget, poll)
    }

    /// Reserve width edits, tokens and all filter output through one runtime allowance.
    pub fn analyze_mapped_budgeted(
        &self,
        input: &FilteredText<'_>,
        limits: KuromojiLimits,
        budget: &MemoryBudget,
        poll: &mut impl FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<Budgeted<AnalyzedText>> {
        input_length(input.as_str(), limits, poll)?;
        let input = CharFilter::CJKWidth.filter_mapped_budgeted(input.clone(), budget, poll)?;
        let mut output = self
            .tokenizer
            .tokenize_mapped_for_filters_budgeted(&input, limits, budget, poll)?;
        for filter in self.filters.iter() {
            output = filter.filter_analyzed_budgeted(output, &self.model, limits, poll)?;
        }
        output.validate_japanese_attributes(poll)?;
        poll()?;
        Ok(output)
    }

    /// Normalize the entire input with width and pinned simple lowercase; no tokenization or stop removal.
    pub fn normalize(&self, input: &str) -> AnalysisResult<String> {
        Ok(self
            .normalize_budgeted(
                input,
                KuromojiLimits::default(),
                &MemoryBudget::new(usize::MAX),
                &mut || Ok(()),
            )?
            .into_parts()
            .0)
    }
    pub fn normalize_budgeted(
        &self,
        input: &str,
        limits: KuromojiLimits,
        budget: &MemoryBudget,
        poll: &mut impl FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<Budgeted<String>> {
        input_length(input, limits, poll)?;
        let filtered = CharFilter::CJKWidth.filter_with_offsets_budgeted(input, budget, poll)?;
        let (mut units, memory) =
            crate::morphology::input::encode(filtered.as_str(), budget, poll, |length| {
                check_limit(
                    "Kuromoji normalization output UTF-16 units",
                    length,
                    limits.max_output_utf16,
                )
                .map_err(Into::into)
            })?
            .into_parts();
        super::filters::lowercase(&mut units, &self.model, &mut Work::new(poll)?)?;
        drop(filtered);
        let (term, memory) =
            TokenTerm::from_utf16_budgeted(Budgeted::new(units, memory), &mut *poll)?.into_parts();
        let text = term.into_string().map_err(|_| {
            super::error::invalid("Kuromoji normalization", "invalid scalar result")
        })?;
        poll()?;
        Ok(Budgeted::new(text, memory))
    }
}
fn input_length(
    input: &str,
    limits: KuromojiLimits,
    poll: &mut dyn FnMut() -> AnalysisResult<()>,
) -> AnalysisResult<usize> {
    crate::morphology::input::utf16_len(input, poll, |length| {
        check_limit(
            "Kuromoji input UTF-16 units",
            length,
            limits.max_input_utf16,
        )
        .map_err(Into::into)
    })
}
