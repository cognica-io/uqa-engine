//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Japanese attribute filters share native/common token ownership and preserve terminal state.

use super::{CompletionMode, KuromojiDictionary, KuromojiLimits, KuromojiOutput};
use crate::morphology::filter::{AllocatedStream, FilterStream};
use crate::{AnalysisResult, AnalyzedText};
use serde::{Deserialize, Serialize};
use uqa_core::memory::{Budgeted, MemoryBudget};

mod kana;
mod kernel;
mod reading;
pub(crate) mod stream;
mod words;
use kana::Kana;
pub(super) use words::lowercase;
use words::PreparedWords;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type")]
pub enum JapaneseFilter {
    #[serde(rename = "kuromoji_baseform")]
    BaseForm,
    #[serde(rename = "kuromoji_part_of_speech")]
    PartOfSpeech { stop_tags: Option<Vec<String>> },
    #[serde(rename = "kuromoji_stop")]
    Stop {
        words: Option<Vec<String>>,
        ignore_case: bool,
    },
    #[serde(rename = "kuromoji_stemmer")]
    KatakanaStem { minimum_length: i32 },
    #[serde(rename = "unicode_simple_lowercase")]
    SimpleLowercase,
    #[serde(rename = "kuromoji_hiragana_uppercase")]
    HiraganaUppercase,
    #[serde(rename = "kuromoji_katakana_uppercase")]
    KatakanaUppercase,
    #[serde(rename = "kuromoji_readingform")]
    ReadingForm { use_romaji: bool },
    #[serde(rename = "kuromoji_number")]
    Number,
    #[serde(rename = "kuromoji_completion")]
    Completion { mode: CompletionMode },
}

#[derive(Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
enum FilterConfig {
    #[serde(rename = "kuromoji_baseform")]
    BaseForm {},
    #[serde(rename = "kuromoji_part_of_speech")]
    PartOfSpeech {
        #[serde(default)]
        stop_tags: Option<Vec<String>>,
    },
    #[serde(rename = "kuromoji_stop")]
    Stop {
        #[serde(default)]
        words: Option<Vec<String>>,
        #[serde(default = "default_ignore_case")]
        ignore_case: bool,
    },
    #[serde(rename = "kuromoji_stemmer")]
    KatakanaStem {
        #[serde(default = "default_stem_length")]
        minimum_length: i32,
    },
    #[serde(rename = "unicode_simple_lowercase")]
    SimpleLowercase {},
    #[serde(rename = "kuromoji_hiragana_uppercase")]
    HiraganaUppercase {},
    #[serde(rename = "kuromoji_katakana_uppercase")]
    KatakanaUppercase {},
    #[serde(rename = "kuromoji_readingform")]
    ReadingForm {
        #[serde(default)]
        use_romaji: bool,
    },
    #[serde(rename = "kuromoji_number")]
    Number {},
    #[serde(rename = "kuromoji_completion")]
    Completion {
        #[serde(default)]
        mode: CompletionMode,
    },
}
fn default_ignore_case() -> bool {
    true
}
pub(super) fn default_stem_length() -> i32 {
    4
}
impl<'de> Deserialize<'de> for JapaneseFilter {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(match FilterConfig::deserialize(deserializer)? {
            FilterConfig::BaseForm {} => Self::BaseForm,
            FilterConfig::PartOfSpeech { stop_tags } => Self::PartOfSpeech { stop_tags },
            FilterConfig::Stop { words, ignore_case } => Self::Stop { words, ignore_case },
            FilterConfig::KatakanaStem { minimum_length } => Self::KatakanaStem { minimum_length },
            FilterConfig::SimpleLowercase {} => Self::SimpleLowercase,
            FilterConfig::HiraganaUppercase {} => Self::HiraganaUppercase,
            FilterConfig::KatakanaUppercase {} => Self::KatakanaUppercase,
            FilterConfig::ReadingForm { use_romaji } => Self::ReadingForm { use_romaji },
            FilterConfig::Number {} => Self::Number,
            FilterConfig::Completion { mode } => Self::Completion { mode },
        })
    }
}

#[derive(Debug)]
pub(super) enum CompiledFilter {
    BaseForm,
    PartOfSpeech(PreparedWords),
    Stop(PreparedWords, bool),
    KatakanaStem(usize),
    SimpleLowercase,
    SmallKana(Kana),
    ReadingForm(bool),
    Number,
    Completion(CompletionMode),
}

impl JapaneseFilter {
    pub(super) fn compile(
        &self,
        model: Option<&KuromojiDictionary>,
        limits: KuromojiLimits,
        budget: &MemoryBudget,
        poll: &mut dyn FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<Budgeted<CompiledFilter>> {
        poll()?;
        let mut memory = budget.empty_reservation();
        let value = match self {
            Self::BaseForm => CompiledFilter::BaseForm,
            Self::SimpleLowercase => {
                dictionary(model)?;
                CompiledFilter::SimpleLowercase
            }
            Self::HiraganaUppercase => CompiledFilter::SmallKana(Kana::Hiragana),
            Self::KatakanaUppercase => CompiledFilter::SmallKana(Kana::Katakana),
            Self::ReadingForm { use_romaji } => CompiledFilter::ReadingForm(*use_romaji),
            Self::Number => CompiledFilter::Number,
            Self::Completion { mode } => {
                dictionary(model)?;
                CompiledFilter::Completion(*mode)
            }
            Self::KatakanaStem { minimum_length } => {
                if *minimum_length < 1 {
                    return Err(super::error::invalid(
                        "Kuromoji stemmer",
                        "minimum length must be at least one",
                    )
                    .into());
                }
                CompiledFilter::KatakanaStem(*minimum_length as usize)
            }
            Self::PartOfSpeech { stop_tags } => {
                let words = match stop_tags.as_deref() {
                    Some(words) => words,
                    None => dictionary(model)?.default_stop_tags(),
                };
                let (words, allocation) =
                    PreparedWords::new(words, false, model, limits, budget, poll)?.into_parts();
                memory.absorb(allocation);
                CompiledFilter::PartOfSpeech(words)
            }
            Self::Stop { words, ignore_case } => {
                if *ignore_case {
                    dictionary(model)?;
                }
                let words = match words.as_deref() {
                    Some(words) => words,
                    None => dictionary(model)?.default_stop_words(),
                };
                let (words, allocation) =
                    PreparedWords::new(words, *ignore_case, model, limits, budget, poll)?
                        .into_parts();
                memory.absorb(allocation);
                CompiledFilter::Stop(words, *ignore_case)
            }
        };
        poll()?;
        Ok(Budgeted::new(value, memory))
    }

    pub fn apply(
        &self,
        input: KuromojiOutput,
        model: &KuromojiDictionary,
    ) -> AnalysisResult<KuromojiOutput> {
        self.apply_controlled(input, model, KuromojiLimits::default(), &mut || Ok(()))
    }

    pub fn apply_controlled(
        &self,
        input: KuromojiOutput,
        model: &KuromojiDictionary,
        limits: KuromojiLimits,
        poll: &mut impl FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<KuromojiOutput> {
        let input = AllocatedStream::from_unreserved(input.into(), poll)?.into_budgeted();
        let (input, memory) = input.into_parts();
        Ok(self
            .apply_budgeted(Budgeted::new(input.into(), memory), model, limits, poll)?
            .into_parts()
            .0)
    }

    /// Consume the complete native reservation; preparation and mutation use the same allowance.
    pub fn apply_budgeted(
        &self,
        input: Budgeted<KuromojiOutput>,
        model: &KuromojiDictionary,
        limits: KuromojiLimits,
        poll: &mut impl FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<Budgeted<KuromojiOutput>> {
        let (input, memory) = input.into_parts();
        let compiled = self.compile(Some(model), limits, memory.budget(), poll)?;
        let output = compiled.apply_budgeted(Budgeted::new(input, memory), model, limits, poll)?;
        output.validate_attributes(poll)?;
        Ok(output)
    }

    /// Apply Japanese rules to common tokens; absent Japanese attributes remain absent.
    pub fn filter_analyzed(
        &self,
        input: AnalyzedText,
        model: &KuromojiDictionary,
    ) -> AnalysisResult<AnalyzedText> {
        self.filter_analyzed_controlled(input, model, KuromojiLimits::default(), &mut || Ok(()))
    }

    pub fn filter_analyzed_controlled(
        &self,
        input: AnalyzedText,
        model: &KuromojiDictionary,
        limits: KuromojiLimits,
        poll: &mut impl FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<AnalyzedText> {
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

    /// Preserve source owners, independent token leases and trailing skipped positions on mutation.
    pub fn filter_analyzed_budgeted(
        &self,
        input: Budgeted<AnalyzedText>,
        model: &KuromojiDictionary,
        limits: KuromojiLimits,
        poll: &mut impl FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<Budgeted<AnalyzedText>> {
        let (input, memory) = input.into_parts();
        let compiled = self.compile(Some(model), limits, memory.budget(), poll)?;
        let output = compiled.filter_analyzed_budgeted(
            Budgeted::new(input, memory),
            Some(model),
            limits,
            poll,
        )?;
        output.validate_japanese_attributes(poll)?;
        Ok(output)
    }
}

impl CompiledFilter {
    pub(super) fn apply_budgeted(
        &self,
        input: Budgeted<KuromojiOutput>,
        model: &KuromojiDictionary,
        limits: KuromojiLimits,
        poll: &mut dyn FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<Budgeted<KuromojiOutput>> {
        let (input, memory) = input.into_parts();
        let stream = AllocatedStream::from_budgeted(Budgeted::new(input.into(), memory));
        let (output, memory) = self
            .apply_owned(stream, Some(model), limits, poll)?
            .into_budgeted()
            .into_parts();
        Ok(Budgeted::new(output.into(), memory))
    }

    pub(super) fn filter_analyzed_budgeted(
        &self,
        input: Budgeted<AnalyzedText>,
        model: Option<&KuromojiDictionary>,
        limits: KuromojiLimits,
        poll: &mut dyn FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<Budgeted<AnalyzedText>> {
        let (input, memory) = input.into_parts();
        let stream = FilterStream {
            tokens: input.batch.tokens,
            terminal: input.batch.terminal,
            final_position_increment: input.batch.final_position_increment,
            final_offset_utf16: input.projection.filtered_len(),
            context: input.projection,
        };
        let (stream, memory) = self
            .apply_owned(
                AllocatedStream::from_budgeted(Budgeted::new(stream, memory)),
                model,
                limits,
                poll,
            )?
            .into_budgeted()
            .into_parts();
        let output = Budgeted::new(
            AnalyzedText {
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
        output.batch.validate_positions_with_control(poll)?;
        poll()?;
        Ok(output)
    }
}

fn dictionary(model: Option<&KuromojiDictionary>) -> AnalysisResult<&KuromojiDictionary> {
    model.ok_or(crate::AnalysisError::Descriptor(
        "Japanese filter requires a dictionary profile",
    ))
}

#[cfg(test)]
mod tests;
