//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Feature-selected stages preserve unresolved options and their original defaults.

#[cfg(any(feature = "nori", feature = "kuromoji"))]
use super::{invalid, Input, JsonToken, Result};

#[cfg(any(feature = "nori", feature = "kuromoji"))]
impl Input<'_, '_> {
    pub(super) fn profile(&mut self) -> Result<crate::UnicodeProfile> {
        self.expect(JsonToken::StartObject)?;
        self.key("provider")?;
        let provider = self.tag()?;
        self.key("dictionary")?;
        let dictionary = self.text()?;
        let value = match provider {
            #[cfg(feature = "nori")]
            "nori" => crate::UnicodeProfile::Nori { dictionary },
            #[cfg(feature = "kuromoji")]
            "kuromoji" => crate::UnicodeProfile::Kuromoji { dictionary },
            _ => return Err(invalid()),
        };
        self.expect(JsonToken::EndObject)?;
        Ok(value)
    }

    pub(super) fn morphology_filter(&mut self, tag: &str) -> Result<crate::TokenFilter> {
        use crate::TokenFilter;
        Ok(match tag {
            #[cfg(feature = "nori")]
            "nori_part_of_speech" => {
                self.key("stop_tags")?;
                TokenFilter::NoriPartOfSpeech(crate::nori::NoriPOSConfig {
                    stop_tags: self.optional(|input| {
                        input.sequence(|input| {
                            crate::nori::POSTag::from_name(input.tag()?).ok_or_else(invalid)
                        })
                    })?,
                })
            }
            #[cfg(feature = "nori")]
            "nori_readingform" => TokenFilter::NoriReadingForm(crate::EmptyFilterConfig {}),
            #[cfg(feature = "nori")]
            "nori_number" => TokenFilter::NoriNumber(crate::EmptyFilterConfig {}),
            "unicode_simple_lowercase" => {
                self.key("unicode_profile")?;
                let unicode_profile = if matches!(self.peek()?, JsonToken::String(_)) {
                    crate::UnicodeProfileSource::LegacyNori(self.text()?)
                } else {
                    crate::UnicodeProfileSource::Explicit(self.profile()?)
                };
                TokenFilter::UnicodeSimpleLowercase(crate::SimpleLowercaseConfig {
                    unicode_profile,
                })
            }
            #[cfg(feature = "kuromoji")]
            tag => self.kuromoji_filter(tag)?,
            #[cfg(not(feature = "kuromoji"))]
            _ => return Err(invalid()),
        })
    }
}

#[cfg(feature = "nori")]
impl Input<'_, '_> {
    pub(super) fn nori_tokenizer(&mut self) -> Result<crate::nori::NoriTokenizerConfig> {
        self.key("dictionary")?;
        let dictionary = self.text()?;
        self.key("decompound_mode")?;
        let decompound_mode = match self.tag()? {
            "none" => crate::nori::DecompoundMode::None,
            "discard" => crate::nori::DecompoundMode::Discard,
            "mixed" => crate::nori::DecompoundMode::Mixed,
            _ => return Err(invalid()),
        };
        self.key("output_unknown_unigrams")?;
        let output_unknown_unigrams = self.boolean()?;
        self.key("discard_punctuation")?;
        let discard_punctuation = self.boolean()?;
        self.key("user_dictionary")?;
        Ok(crate::nori::NoriTokenizerConfig {
            dictionary,
            decompound_mode,
            output_unknown_unigrams,
            discard_punctuation,
            user_dictionary: self.optional(Self::text)?,
        })
    }
}

#[cfg(feature = "kuromoji")]
impl Input<'_, '_> {
    pub(super) fn kuromoji_tokenizer(
        &mut self,
    ) -> Result<crate::kuromoji::KuromojiTokenizerConfig> {
        self.key("dictionary")?;
        let dictionary = self.text()?;
        self.key("mode")?;
        let mode = match self.tag()? {
            "normal" => crate::kuromoji::KuromojiMode::Normal,
            "search" => crate::kuromoji::KuromojiMode::Search,
            "extended" => crate::kuromoji::KuromojiMode::Extended,
            _ => return Err(invalid()),
        };
        self.key("discard_punctuation")?;
        let discard_punctuation = self.boolean()?;
        self.key("discard_compound_token")?;
        let discard_compound_token = self.boolean()?;
        self.key("user_dictionary")?;
        let user_dictionary = self.optional(Self::text)?;
        self.key("n_best_cost")?;
        let n_best_cost = self.number()?;
        self.key("n_best_examples")?;
        Ok(crate::kuromoji::KuromojiTokenizerConfig {
            dictionary,
            mode,
            discard_punctuation,
            discard_compound_token,
            user_dictionary,
            n_best_cost,
            n_best_examples: self.optional(Self::text)?,
        })
    }

    fn kuromoji_filter(&mut self, tag: &str) -> Result<crate::TokenFilter> {
        use crate::{kuromoji, EmptyFilterConfig, TokenFilter};
        Ok(match tag {
            "kuromoji_part_of_speech" => {
                self.key("stop_tags")?;
                let stop_tags = self.optional(Self::strings)?;
                self.key("dictionary")?;
                TokenFilter::KuromojiPartOfSpeech(kuromoji::KuromojiPOSConfig {
                    stop_tags,
                    dictionary: self.optional(Self::text)?,
                })
            }
            "kuromoji_stop" => {
                self.key("words")?;
                let words = self.optional(Self::strings)?;
                self.key("ignore_case")?;
                let ignore_case = self.boolean()?;
                self.key("dictionary")?;
                TokenFilter::KuromojiStop(kuromoji::KuromojiStopConfig {
                    words,
                    ignore_case,
                    dictionary: self.optional(Self::text)?,
                })
            }
            "kuromoji_completion" => {
                self.key("dictionary")?;
                let dictionary = self.text()?;
                self.key("mode")?;
                let mode = match self.tag()? {
                    "index" => kuromoji::CompletionMode::Index,
                    "query" => kuromoji::CompletionMode::Query,
                    _ => return Err(invalid()),
                };
                TokenFilter::KuromojiCompletion(kuromoji::KuromojiCompletionConfig {
                    dictionary,
                    mode,
                })
            }
            "kuromoji_baseform" => TokenFilter::KuromojiBaseForm(EmptyFilterConfig {}),
            "kuromoji_stemmer" => {
                self.key("minimum_length")?;
                TokenFilter::KuromojiStem(kuromoji::KuromojiStemConfig {
                    minimum_length: self.number()?,
                })
            }
            "kuromoji_hiragana_uppercase" => {
                TokenFilter::KuromojiHiraganaUppercase(EmptyFilterConfig {})
            }
            "kuromoji_katakana_uppercase" => {
                TokenFilter::KuromojiKatakanaUppercase(EmptyFilterConfig {})
            }
            "kuromoji_readingform" => {
                self.key("use_romaji")?;
                TokenFilter::KuromojiReadingForm(kuromoji::KuromojiReadingFormConfig {
                    use_romaji: self.boolean()?,
                })
            }
            "kuromoji_number" => TokenFilter::KuromojiNumber(EmptyFilterConfig {}),
            _ => return Err(invalid()),
        })
    }
}
