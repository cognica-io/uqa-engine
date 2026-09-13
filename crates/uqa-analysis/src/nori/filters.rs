//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Attribute-preserving Korean filters over lossless UTF-16 token streams.

use serde::{Deserialize, Serialize};

use super::error::{check_limit, invalid};
use super::{NoriDictionary, NoriLimits, NoriOutput, POSTag};
use crate::AnalysisResult;

mod lowercase;
pub(crate) mod stream;
#[cfg(test)]
mod tests;

use stream::{AllocatedStream, FilterStream, FilterToken};
use uqa_core::memory::{Budgeted, BudgetedVec, MemoryBudget};

pub const DEFAULT_STOP_TAGS: &[POSTag] = &[
    POSTag::EP,
    POSTag::EF,
    POSTag::EC,
    POSTag::ETN,
    POSTag::ETM,
    POSTag::IC,
    POSTag::JKS,
    POSTag::JKC,
    POSTag::JKG,
    POSTag::JKO,
    POSTag::JKB,
    POSTag::JKV,
    POSTag::JKQ,
    POSTag::JX,
    POSTag::JC,
    POSTag::MAG,
    POSTag::MAJ,
    POSTag::MM,
    POSTag::SP,
    POSTag::SSC,
    POSTag::SSO,
    POSTag::SC,
    POSTag::SE,
    POSTag::XPN,
    POSTag::XSA,
    POSTag::XSN,
    POSTag::XSV,
    POSTag::UNA,
    POSTag::NA,
    POSTag::VSV,
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type")]
pub enum KoreanFilter {
    #[serde(rename = "nori_part_of_speech")]
    PartOfSpeech {
        /// Omission uses Lucene's default set; an empty list retains every POS tag.
        #[serde(default)]
        stop_tags: Option<Vec<POSTag>>,
    },
    #[serde(rename = "nori_readingform")]
    ReadingForm,
    #[serde(rename = "unicode_simple_lowercase")]
    SimpleLowercase,
    #[serde(rename = "nori_number")]
    /// Optional exact number composition, including shared lookahead attributes and keyword handling.
    Number,
}

// Empty struct variants validate that parameterless filters have no extra properties.
#[derive(Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
enum FilterConfig {
    #[serde(rename = "nori_part_of_speech")]
    PartOfSpeech {
        #[serde(default)]
        stop_tags: Option<Vec<POSTag>>,
    },
    #[serde(rename = "nori_readingform")]
    ReadingForm {},
    #[serde(rename = "unicode_simple_lowercase")]
    SimpleLowercase {},
    #[serde(rename = "nori_number")]
    Number {},
}

impl<'de> Deserialize<'de> for KoreanFilter {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(match FilterConfig::deserialize(deserializer)? {
            FilterConfig::PartOfSpeech { stop_tags } => Self::PartOfSpeech { stop_tags },
            FilterConfig::ReadingForm {} => Self::ReadingForm,
            FilterConfig::SimpleLowercase {} => Self::SimpleLowercase,
            FilterConfig::Number {} => Self::Number,
        })
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum CompiledFilter {
    PartOfSpeech(u64),
    ReadingForm,
    SimpleLowercase,
    Number,
}

impl KoreanFilter {
    pub(crate) fn compile(&self) -> CompiledFilter {
        match self {
            Self::PartOfSpeech { stop_tags } => CompiledFilter::PartOfSpeech(
                stop_tags
                    .as_deref()
                    .unwrap_or(DEFAULT_STOP_TAGS)
                    .iter()
                    .fold(0, |bits, tag| bits | (1_u64 << tag.ordinal())),
            ),
            Self::ReadingForm => CompiledFilter::ReadingForm,
            Self::SimpleLowercase => CompiledFilter::SimpleLowercase,
            Self::Number => CompiledFilter::Number,
        }
    }

    pub fn apply(&self, input: NoriOutput, model: &NoriDictionary) -> AnalysisResult<NoriOutput> {
        self.apply_controlled(input, model, NoriLimits::default(), &mut || Ok(()))
    }

    pub fn apply_controlled(
        &self,
        input: NoriOutput,
        model: &NoriDictionary,
        limits: NoriLimits,
        poll: &mut impl FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<NoriOutput> {
        self.compile().apply(input, model, limits, poll)
    }

    /// Consume reserved native tokens and retain one allowance through filtering and numeric composition.
    ///
    /// Use the complete reservation returned by native budgeted tokenization. Input/output vectors, terms, morphology, hidden terminal attributes and numeric scratch keep their allocation owners until destruction. Count-limit, byte-limit and callback failures return no partial result.
    pub fn apply_budgeted(
        &self,
        input: Budgeted<NoriOutput>,
        model: &NoriDictionary,
        limits: NoriLimits,
        poll: &mut impl FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<Budgeted<NoriOutput>> {
        self.compile().apply_budgeted(input, model, limits, poll)
    }

    /// Apply the same Korean filter to a complete common token stream.
    ///
    /// Absent Korean morphology stays absent: POS filtering retains such tokens and reading conversion leaves their terms unchanged. Number composition retains the reference's lookahead metadata and projects its composed span through the original character filters.
    ///
    /// ```
    /// use uqa_analysis::{Tokenizer, nori::{DictionaryLimits, KoreanFilter, NoriDictionary}};
    /// let model = NoriDictionary::from_bytes(uqa_nori_data::BUNDLE, DictionaryLimits::default())?;
    /// let input = Tokenizer::Whitespace.tokenize_with_offsets("３ ． ２ 천 원")?;
    /// let result = KoreanFilter::Number.filter_analyzed(input, &model)?;
    /// assert_eq!(result.tokens()[0].term(), "3200");
    /// assert_eq!(result.tokens()[0].offsets().unwrap().utf16, 0..7);
    /// assert!(result.tokens()[0].korean_morphology().is_none());
    /// # Ok::<(), uqa_analysis::AnalysisError>(())
    /// ```
    pub fn filter_analyzed(
        &self,
        input: crate::AnalyzedText,
        model: &NoriDictionary,
    ) -> AnalysisResult<crate::AnalyzedText> {
        self.filter_analyzed_controlled(input, model, NoriLimits::default(), &mut || Ok(()))
    }

    /// Apply a common-stream filter with bounds and cancellation; failures publish no partial output.
    pub fn filter_analyzed_controlled(
        &self,
        input: crate::AnalyzedText,
        model: &NoriDictionary,
        limits: NoriLimits,
        poll: &mut impl FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<crate::AnalyzedText> {
        Ok(self
            .filter_analyzed_budgeted(
                input.into_unlimited_with_control(poll)?,
                model,
                limits,
                poll,
            )?
            .into_parts()
            .0)
    }

    /// Filter a reserved common stream with count limits, allocation ownership, and cancellation.
    ///
    /// Existing source projections retain their shared allocation leases. All copied token buffers receive independent reservations from the input allowance, including numeric lookahead snapshots and hidden terminal state.
    pub fn filter_analyzed_budgeted(
        &self,
        input: Budgeted<crate::AnalyzedText>,
        model: &NoriDictionary,
        limits: NoriLimits,
        poll: &mut impl FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<Budgeted<crate::AnalyzedText>> {
        self.compile()
            .filter_analyzed_budgeted(input, Some(model), limits, poll)
    }
}

impl CompiledFilter {
    pub(crate) fn apply_budgeted(
        self,
        input: Budgeted<NoriOutput>,
        model: &NoriDictionary,
        limits: NoriLimits,
        poll: &mut impl FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<Budgeted<NoriOutput>> {
        let (input, memory) = input.into_parts();
        let (output, memory) = self
            .apply_stream_budgeted(
                Budgeted::new(input.into(), memory),
                Some(model),
                limits,
                poll,
            )?
            .into_parts();
        Ok(Budgeted::new(output.into(), memory))
    }

    pub(crate) fn filter_analyzed_budgeted(
        self,
        input: Budgeted<crate::AnalyzedText>,
        model: Option<&NoriDictionary>,
        limits: NoriLimits,
        poll: &mut impl FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<Budgeted<crate::AnalyzedText>> {
        let (input, memory) = input.into_parts();
        let stream = FilterStream {
            tokens: input.batch.tokens,
            terminal: input.batch.terminal,
            final_position_increment: input.batch.final_position_increment,
            final_offset_utf16: input.projection.filtered_len(),
            context: input.projection,
        };
        let (stream, memory) = self
            .apply_stream_budgeted(Budgeted::new(stream, memory), model, limits, poll)?
            .into_parts();
        let result = Budgeted::new(
            crate::AnalyzedText {
                batch: crate::token::TokenBatch {
                    tokens: stream.tokens,
                    terminal: stream.terminal,
                    final_position_increment: stream.final_position_increment,
                },
                projection: stream.context,
                final_offsets: input.final_offsets,
            },
            memory,
        );
        result.batch.validate_positions_with_control(poll)?;
        poll()?;
        Ok(result)
    }
    pub fn apply(
        self,
        input: NoriOutput,
        model: &NoriDictionary,
        limits: NoriLimits,
        poll: &mut impl FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<NoriOutput> {
        Ok(self
            .apply_stream(input.into(), Some(model), limits, poll)?
            .into())
    }

    pub(crate) fn apply_stream<T: FilterToken>(
        self,
        input: FilterStream<T>,
        model: Option<&NoriDictionary>,
        limits: NoriLimits,
        poll: &mut impl FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<FilterStream<T>> {
        Ok(self
            .apply_owned(
                AllocatedStream::from_unreserved(input, poll)?,
                model,
                limits,
                poll,
            )?
            .into_budgeted()
            .into_parts()
            .0)
    }

    pub(crate) fn apply_stream_budgeted<T: FilterToken>(
        self,
        input: Budgeted<FilterStream<T>>,
        model: Option<&NoriDictionary>,
        limits: NoriLimits,
        poll: &mut impl FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<Budgeted<FilterStream<T>>> {
        Ok(self
            .apply_owned(AllocatedStream::from_budgeted(input), model, limits, poll)?
            .into_budgeted())
    }

    fn apply_owned<T: FilterToken>(
        self,
        mut input: AllocatedStream<T>,
        model: Option<&NoriDictionary>,
        limits: NoriLimits,
        poll: &mut impl FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<AllocatedStream<T>> {
        if matches!(self, Self::Number) {
            return super::number::filter(input, limits, poll);
        }
        poll()?;
        check_limit(
            "Nori output tokens",
            input.batch.tokens().len(),
            limits.max_tokens,
        )?;
        check_limit(
            "Nori input UTF-16 units",
            input.final_offset_utf16,
            limits.max_input_utf16,
        )?;
        let mut output_units = 0usize;
        input.batch = if let Self::PartOfSpeech(bits) = self {
            input.batch.retain(
                |token, poll| {
                    if token
                        .left_pos()
                        .is_some_and(|tag| bits & (1_u64 << tag.ordinal()) != 0)
                    {
                        return Ok(false);
                    }
                    let mut work = Work::new(poll)?;
                    let term_units = token.term_len(&mut work)?;
                    output_units =
                        filter_units(token, term_units, output_units, limits, &mut work)?;
                    Ok(true)
                },
                poll,
            )?
        } else {
            let mut work = Work::new(poll)?;
            input.batch.map_tokens(|token, memory| {
                work.tick()?;
                let term_units = if matches!(self, Self::ReadingForm) {
                    if let Some(reading) = token.reading() {
                        text_units(reading, &mut work)?
                    } else {
                        token.term_len(&mut work)?
                    }
                } else {
                    token.term_len(&mut work)?
                };
                output_units = filter_units(token, term_units, output_units, limits, &mut work)?;
                match self {
                    Self::ReadingForm => {
                        token.reading_form(term_units, memory, &input.context, &mut work)?;
                    }
                    Self::SimpleLowercase => {
                        token.lowercase(model, memory, &input.context, &mut work)?;
                    }
                    Self::PartOfSpeech(_) | Self::Number => unreachable!("non-removing filter"),
                }
                Ok(())
            })?
        };
        if let Some(terminal) = input.batch.terminal() {
            let mut work = Work::new(poll)?;
            let term_units = terminal.term_len(&mut work)?;
            filter_units(terminal, term_units, output_units, limits, &mut work)?;
        }
        poll()?;
        Ok(input)
    }
}

fn filter_units<T: FilterToken>(
    token: &T,
    term_units: usize,
    previous: usize,
    limits: NoriLimits,
    work: &mut Work<'_>,
) -> AnalysisResult<usize> {
    let units = previous
        .checked_add(token_units(token, term_units, work)?)
        .ok_or_else(|| invalid("Nori filter", "UTF-16 output size overflow"))?;
    check_limit("Nori output UTF-16 units", units, limits.max_output_utf16)?;
    Ok(units)
}

fn text_units(text: &str, work: &mut Work<'_>) -> AnalysisResult<usize> {
    let mut length = 0;
    for character in text.chars() {
        work.tick()?;
        length += character.len_utf16();
    }
    Ok(length)
}

pub(super) fn token_units<T: FilterToken>(
    token: &T,
    term_units: usize,
    work: &mut Work<'_>,
) -> AnalysisResult<usize> {
    let mut units = term_units;
    if let Some(reading) = token.reading() {
        units = units
            .checked_add(text_units(reading, work)?)
            .ok_or_else(|| invalid("Nori filter", "reading size overflow"))?;
    }
    for part in token.morphemes().into_iter().flatten() {
        work.tick()?;
        units = units
            .checked_add(part.surface_utf16.len())
            .ok_or_else(|| invalid("Nori filter", "morpheme size overflow"))?;
    }
    Ok(units)
}

pub(crate) struct Work<'a> {
    counter: usize,
    pub(crate) poll: &'a mut dyn FnMut() -> AnalysisResult<()>,
}

impl<'a> Work<'a> {
    pub fn new(poll: &'a mut dyn FnMut() -> AnalysisResult<()>) -> AnalysisResult<Self> {
        poll()?;
        Ok(Self { counter: 0, poll })
    }
    pub fn tick(&mut self) -> AnalysisResult<()> {
        self.counter = (self.counter + 1) % 1024;
        if self.counter == 0 {
            (self.poll)()?;
        }
        Ok(())
    }
    pub fn finish(&mut self) -> AnalysisResult<()> {
        (self.poll)()
    }
}

pub(super) fn normalize_budgeted(
    input: &[u16],
    model: &NoriDictionary,
    limits: NoriLimits,
    budget: &MemoryBudget,
    poll: &mut impl FnMut() -> AnalysisResult<()>,
) -> AnalysisResult<Budgeted<Vec<u16>>> {
    let mut work = Work::new(poll)?;
    check_limit(
        "Nori input UTF-16 units",
        input.len(),
        limits.max_input_utf16,
    )?;
    check_limit(
        "Nori output UTF-16 units",
        input.len(),
        limits.max_output_utf16,
    )?;
    let mut output = BudgetedVec::new(budget);
    output.reserve(input.len())?;
    for unit in input {
        work.tick()?;
        output.push(*unit)?;
    }
    lowercase::apply(&mut output, Some(model), &mut work)?;
    (work.poll)()?;
    let (output, memory) = output.into_parts();
    Ok(Budgeted::new(output, memory))
}

pub(super) fn normalize_text_budgeted(
    input: &str,
    model: &NoriDictionary,
    limits: NoriLimits,
    budget: &MemoryBudget,
    poll: &mut impl FnMut() -> AnalysisResult<()>,
) -> AnalysisResult<Budgeted<String>> {
    let input = super::tokenizer::allocation::encode(input, limits.max_input_utf16, budget, poll)?;
    let output = normalize_budgeted(&input, model, limits, budget, poll)?;
    drop(input);
    let (term, memory) = crate::TokenTerm::from_utf16_budgeted(output, &mut *poll)?.into_parts();
    let text = term
        .into_string()
        .map_err(|_| invalid("Nori normalization", "invalid scalar result"))?;
    Ok(Budgeted::new(text, memory))
}
