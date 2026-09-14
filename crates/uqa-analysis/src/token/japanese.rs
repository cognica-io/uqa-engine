//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Japanese morphology enters common graph and source owners without copying dictionary attributes.

use uqa_core::memory::{Budgeted, MemoryBudget};

use super::{native, AnalysisToken, Morphology, TokenBatch};
use crate::kuromoji::filters::stream::JapaneseToken;
use crate::kuromoji::{
    JapaneseMorphology, JapaneseTokenizer, KuromojiLimits, KuromojiOutput, KuromojiToken,
};
use crate::morphology::filter::{ComposingToken, Work};
use crate::TokenTerm;
use crate::{AnalysisResult, AnalyzedText, FilteredText};

impl native::NativeToken for KuromojiToken {
    fn take_term(&mut self) -> Vec<u16> {
        std::mem::take(&mut self.term_utf16)
    }

    fn into_fields(self) -> native::NativeFields {
        let has_morphology = self.origin.is_some() || self.fields().iter().any(Option::is_some);
        native::NativeFields {
            span: self.start_utf16..self.end_utf16,
            increment: self.position_increment,
            length: self.position_length,
            keyword: self.keyword,
            morphology: has_morphology.then_some(Morphology::Japanese(JapaneseMorphology {
                part_of_speech: self.part_of_speech,
                base_form: self.base_form,
                reading: self.reading,
                pronunciation: self.pronunciation,
                inflection_type: self.inflection_type,
                inflection_form: self.inflection_form,
                origin: self.origin,
                errors: self.errors,
            })),
        }
    }
}

impl KuromojiOutput {
    /// Convert tokens over `input.as_str()` into common tokens with corrected original offsets.
    pub fn into_analyzed(self, input: &FilteredText<'_>) -> AnalysisResult<AnalyzedText> {
        self.validate_attributes(&mut || Ok(()))?;
        native::output(
            TokenBatch {
                tokens: self.tokens,
                final_position_increment: self.final_position_increment,
                terminal: self.terminal,
            },
            self.final_offset_utf16,
            input,
        )
    }
}

impl JapaneseTokenizer {
    /// Tokenize character-filter output and retain its original source spans and Japanese attributes.
    ///
    /// ```
    /// use uqa_analysis::CharFilter;
    /// use uqa_analysis::kuromoji::{JapaneseTokenizer, KuromojiOptions, KuromojiResources};
    /// let dictionary = KuromojiResources::default().load_default()?;
    /// let tokenizer = JapaneseTokenizer::new(dictionary.model().clone(), None, KuromojiOptions::default())?;
    /// let input = CharFilter::CJKWidth.filter_with_offsets("ｶﾞ")?;
    /// let output = tokenizer.tokenize_mapped(&input)?;
    /// assert_eq!(output.tokens()[0].term().as_str(), Some("ガ"));
    /// assert_eq!(output.tokens()[0].offsets().unwrap().utf16, 0..2);
    /// assert!(output.tokens()[0].japanese_morphology().is_some());
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn tokenize_mapped(&self, input: &FilteredText<'_>) -> AnalysisResult<AnalyzedText> {
        Ok(self
            .tokenize_mapped_budgeted(
                input,
                KuromojiLimits::default(),
                &MemoryBudget::new(usize::MAX),
                &mut || Ok(()),
            )?
            .into_parts()
            .0)
    }

    /// Reserve native/common tokens and retained source through one allowance without changing borrowed coordinate caches on failure.
    pub fn tokenize_mapped_budgeted(
        &self,
        input: &FilteredText<'_>,
        limits: KuromojiLimits,
        budget: &MemoryBudget,
        poll: &mut impl FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<Budgeted<AnalyzedText>> {
        self.tokenize_mapped_impl(input, limits, budget, poll, false)
    }

    pub(crate) fn tokenize_mapped_for_filters_budgeted(
        &self,
        input: &FilteredText<'_>,
        limits: KuromojiLimits,
        budget: &MemoryBudget,
        poll: &mut impl FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<Budgeted<AnalyzedText>> {
        self.tokenize_mapped_impl(input, limits, budget, poll, true)
    }

    fn tokenize_mapped_impl(
        &self,
        input: &FilteredText<'_>,
        limits: KuromojiLimits,
        budget: &MemoryBudget,
        poll: &mut impl FnMut() -> AnalysisResult<()>,
        deferred: bool,
    ) -> AnalysisResult<Budgeted<AnalyzedText>> {
        let input = input.clone();
        input.prepare_coordinates(budget, poll)?;
        let output = if deferred {
            self.tokenize_for_filters_budgeted(input.as_str(), limits, budget, poll)?
        } else {
            self.tokenize_budgeted(input.as_str(), limits, budget, poll)?
        };
        let (output, memory) = output.into_parts();
        native::output_budgeted(
            TokenBatch {
                tokens: output.tokens,
                final_position_increment: output.final_position_increment,
                terminal: output.terminal,
            },
            output.final_offset_utf16,
            memory,
            &input,
            poll,
        )
    }
}

#[cfg(test)]
mod tests;

impl AnalyzedText {
    pub(crate) fn validate_japanese_attributes(
        &self,
        poll: &mut dyn FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<()> {
        for token in self.tokens() {
            poll()?;
            if let Some(value) = token.japanese_morphology() {
                value.errors.validate()?;
            }
        }
        Ok(())
    }
}

impl JapaneseToken for AnalysisToken {
    fn generated(
        term: Budgeted<Vec<u16>>,
        first: &Self::Span,
        last: &Self::Span,
        increment: u32,
        context: &Self::Context,
        work: &mut Work<'_>,
    ) -> AnalysisResult<Budgeted<Self>> {
        let (term, memory) = TokenTerm::from_utf16_budgeted(term, &mut *work.poll)?.into_parts();
        let mut token = Self {
            term,
            offsets: None,
            position_increment: increment,
            position_length: 1,
            keyword: false,
            filtered_utf16: None,
            morphology: None,
            verbatim: false,
        };
        token.cover(first, last, context, work)?;
        Ok(Budgeted::new(token, memory))
    }

    fn reading(&self) -> AnalysisResult<Option<&str>> {
        let Some(value) = self.japanese_morphology() else {
            return Ok(None);
        };
        value.errors.check(2)?;
        Ok(value.reading.as_deref())
    }
    fn part_of_speech(&self) -> AnalysisResult<Option<&str>> {
        let Some(value) = self.japanese_morphology() else {
            return Ok(None);
        };
        value.errors.check(0)?;
        Ok(value.part_of_speech.as_deref())
    }
    fn attributes(&self) -> [Option<&str>; 6] {
        self.japanese_morphology().map_or([None; 6], |value| {
            value.fields().map(|field| field.map(String::as_str))
        })
    }
}
