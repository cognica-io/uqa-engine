//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Attribute-preserving Korean filters over lossless UTF-16 token streams.

use serde::{Deserialize, Serialize};

use super::error::{check_limit, invalid};
use super::{NoriDictionary, NoriLimits, NoriOutput, POSTag};
use crate::{AnalysisError, AnalysisResult};

mod lowercase;
pub(crate) mod stream;

use stream::{FilterStream, FilterToken};

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
        let stream = FilterStream {
            tokens: input.batch.tokens,
            terminal: input.batch.terminal,
            final_position_increment: input.batch.final_position_increment,
            final_offset_utf16: input.projection.filtered_len(),
            context: input.projection.clone(),
        };
        let stream = self
            .compile()
            .apply_stream(stream, Some(model), limits, poll)?;
        let batch = crate::token::TokenBatch {
            tokens: stream.tokens,
            terminal: stream.terminal,
            final_position_increment: stream.final_position_increment,
        };
        batch.validate_positions()?;
        Ok(crate::AnalyzedText {
            batch,
            projection: input.projection,
            final_offsets: input.final_offsets,
        })
    }
}

impl CompiledFilter {
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
        mut input: FilterStream<T>,
        model: Option<&NoriDictionary>,
        limits: NoriLimits,
        poll: &mut impl FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<FilterStream<T>> {
        if matches!(self, Self::Number) {
            return super::number::filter(input, limits, poll);
        }
        let mut work = Work::new(poll)?;
        check_limit("Nori output tokens", input.tokens.len(), limits.max_tokens)?;
        check_limit(
            "Nori input UTF-16 units",
            input.final_offset_utf16,
            limits.max_input_utf16,
        )?;
        let mut skipped = 0_u32;
        let mut output_units = 0_usize;
        let mut retained = 0;
        let mut trailing_removed = false;
        for index in 0..input.tokens.len() {
            work.tick()?;
            let token = &mut input.tokens[index];
            if let Self::PartOfSpeech(bits) = self {
                if token
                    .left_pos()
                    .is_some_and(|tag| bits & (1_u64 << tag.ordinal()) != 0)
                {
                    trailing_removed = true;
                    skipped = skipped
                        .checked_add(token.increment())
                        .ok_or(AnalysisError::TokenPositionOverflow)?;
                    continue;
                }
                token.set_increment(
                    token
                        .increment()
                        .checked_add(skipped)
                        .ok_or(AnalysisError::TokenPositionOverflow)?,
                );
                skipped = 0;
                trailing_removed = false;
            }
            let term_units = if matches!(self, Self::ReadingForm) {
                token
                    .reading()
                    .map_or(token.term_len(), |text| text.encode_utf16().count())
            } else {
                token.term_len()
            };
            output_units = output_units
                .checked_add(token_units(token, term_units, &mut work)?)
                .ok_or_else(|| invalid("Nori filter", "UTF-16 output size overflow"))?;
            check_limit(
                "Nori output UTF-16 units",
                output_units,
                limits.max_output_utf16,
            )?;
            match self {
                Self::ReadingForm => token.mutate_term(&input.context, |term, reading| {
                    if let Some(reading) = reading {
                        term.clear();
                        term.try_reserve(term_units)
                            .map_err(super::DictionaryError::from)?;
                        for unit in reading.encode_utf16() {
                            work.tick()?;
                            term.push(unit);
                        }
                    }
                    Ok(())
                })?,
                Self::SimpleLowercase => token.mutate_term(&input.context, |term, _| {
                    lowercase::apply(term, model, &mut work)
                })?,
                Self::PartOfSpeech(_) | Self::Number => {}
            }
            input.tokens.swap(retained, index);
            retained += 1;
        }
        if trailing_removed && input.terminal.is_none() {
            // A filtering stream can change shared attributes while returning false at EOF.
            input.terminal = input.tokens.pop().map(Box::new);
        }
        input.tokens.truncate(retained);
        if let Some(terminal) = &input.terminal {
            output_units = output_units
                .checked_add(token_units(
                    terminal.as_ref(),
                    terminal.term_len(),
                    &mut work,
                )?)
                .ok_or_else(|| invalid("Nori filter", "terminal attribute size overflow"))?;
            check_limit(
                "Nori output UTF-16 units",
                output_units,
                limits.max_output_utf16,
            )?;
        }
        input.final_position_increment = input
            .final_position_increment
            .checked_add(skipped)
            .ok_or(AnalysisError::TokenPositionOverflow)?;
        (work.poll)()?;
        Ok(input)
    }
}

pub(super) fn token_units<T: FilterToken>(
    token: &T,
    term_units: usize,
    work: &mut Work<'_>,
) -> AnalysisResult<usize> {
    let mut units = term_units;
    if let Some(reading) = token.reading() {
        units = units
            .checked_add(reading.encode_utf16().count())
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

pub(super) struct Work<'a> {
    counter: usize,
    poll: &'a mut dyn FnMut() -> AnalysisResult<()>,
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

pub(super) fn normalize(
    input: &[u16],
    model: &NoriDictionary,
    limits: NoriLimits,
    poll: &mut impl FnMut() -> AnalysisResult<()>,
) -> AnalysisResult<Vec<u16>> {
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
    let mut output = super::io::vector(input.len())?;
    output.extend_from_slice(input);
    lowercase::apply(&mut output, Some(model), &mut work)?;
    (work.poll)()?;
    Ok(output)
}
